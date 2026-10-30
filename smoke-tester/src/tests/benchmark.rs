//! Probe throughput benchmarks.
//!
//! Set `SMOKE_TESTER_BENCH_OUT` to a file path to measure. The results are appended to the file
//! as JSON lines. Without it, each operation runs once as a test.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    time::{Duration, Instant},
};

use anyhow::Context;
use linkme::distributed_slice;
use probe_rs::{Core, MemoryInterface, Session, config::MemoryRegion};
use serde::Serialize;

use crate::{SESSION_TESTS, TestResult, dut_definition::DutDefinition, skip_test};

const OUTPUT_ENV: &str = "SMOKE_TESTER_BENCH_OUT";

/// Minimum total time of the samples of one measurement.
const SAMPLE_TIME: Duration = Duration::from_millis(200);
const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 1000;

/// Bulk transfer sizes in bytes.
const BULK_SIZES: [usize; 2] = [64, 16 * 1024];

#[derive(Serialize)]
struct Record<'a> {
    chip: &'a str,
    probe: &'a str,
    protocol: Option<&'a str>,
    speed_khz: u32,
    metric: &'static str,
    size: usize,
    unit: &'static str,
    improves: Improves,
    median: f64,
    p10: f64,
    p90: f64,
    samples: usize,
}

/// The direction in which a metric gets better.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Improves {
    Up,
    Down,
}

struct Bench {
    output: Option<File>,
    chip: String,
    probe: String,
    protocol: Option<String>,
    speed_khz: u32,
}

impl Bench {
    /// Run `op` once, then again until enough samples are collected if the benchmark measures.
    ///
    /// `op` returns the time of the part to measure.
    fn sample(&self, mut op: impl FnMut() -> anyhow::Result<Duration>) -> anyhow::Result<Vec<f64>> {
        op()?;

        let mut samples = Vec::new();
        if self.output.is_none() {
            return Ok(samples);
        }

        let mut total = Duration::ZERO;
        while samples.len() < MAX_SAMPLES && (samples.len() < MIN_SAMPLES || total < SAMPLE_TIME) {
            let time = op()?;
            total += time;
            samples.push(time.as_secs_f64());
        }

        Ok(samples)
    }

    /// Write the statistics of `values` to the output file.
    fn record(
        &mut self,
        metric: &'static str,
        size: usize,
        unit: &'static str,
        improves: Improves,
        mut values: Vec<f64>,
    ) -> anyhow::Result<()> {
        let Some(output) = &mut self.output else {
            return Ok(());
        };

        values.sort_by(f64::total_cmp);
        let percentile = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];

        let record = Record {
            chip: &self.chip,
            probe: &self.probe,
            protocol: self.protocol.as_deref(),
            speed_khz: self.speed_khz,
            metric,
            size,
            unit,
            improves,
            median: percentile(0.5),
            p10: percentile(0.1),
            p90: percentile(0.9),
            samples: values.len(),
        };

        let line = serde_json::to_string(&record)?;
        println!("{line}");
        writeln!(output, "{line}")?;

        Ok(())
    }

    /// Measure the time of single accesses in microseconds.
    fn latency(&mut self, core: &mut Core, address: u64) -> anyhow::Result<()> {
        let samples = self.sample(|| {
            let start = Instant::now();
            core.read_word_32(address)?;
            Ok(start.elapsed())
        })?;
        self.record("read_word_32", 4, "us", Improves::Down, to_micros(samples))?;

        let pc = core.program_counter();
        let samples = self.sample(|| {
            let start = Instant::now();
            core.read_core_reg::<u64>(pc)?;
            Ok(start.elapsed())
        })?;
        self.record("read_core_reg", 0, "us", Improves::Down, to_micros(samples))
    }

    /// Measure the throughput of block accesses in bytes per second.
    fn bulk(&mut self, core: &mut Core, address: u64, size: usize) -> anyhow::Result<()> {
        let mut data = vec![0; size / 4];
        let mut readback = vec![0; size / 4];
        let mut seed = 0;

        let samples = self.sample(|| {
            seed += 1;
            fill(&mut data, seed);

            let start = Instant::now();
            core.write_32(address, &data)?;
            core.flush()?;
            let time = start.elapsed();

            core.read_32(address, &mut readback)?;
            verify(&data, &readback, address)?;
            Ok(time)
        })?;
        self.record(
            "write_32",
            size,
            "B/s",
            Improves::Up,
            to_throughput(samples, size),
        )?;

        let samples = self.sample(|| {
            readback.fill(0);

            let start = Instant::now();
            core.read_32(address, &mut readback)?;
            let time = start.elapsed();

            verify(&data, &readback, address)?;
            Ok(time)
        })?;
        self.record(
            "read_32",
            size,
            "B/s",
            Improves::Up,
            to_throughput(samples, size),
        )
    }
}

/// Fill `data` with a pattern that differs for each `seed`.
fn fill(data: &mut [u32], seed: u32) {
    for (i, word) in data.iter_mut().enumerate() {
        *word = (i as u32 ^ seed.rotate_left(16)).wrapping_mul(0x9E37_79B9);
    }
}

fn verify(expected: &[u32], actual: &[u32], address: u64) -> anyhow::Result<()> {
    if let Some(i) = expected.iter().zip(actual).position(|(e, a)| e != a) {
        anyhow::bail!(
            "Readback differs at address {:#010X}: expected {:#010X}, got {:#010X}",
            address + 4 * i as u64,
            expected[i],
            actual[i]
        );
    }
    Ok(())
}

fn to_micros(samples: Vec<f64>) -> Vec<f64> {
    samples.into_iter().map(|s| s * 1e6).collect()
}

fn to_throughput(samples: Vec<f64>, size: usize) -> Vec<f64> {
    samples.into_iter().map(|s| size as f64 / s).collect()
}

#[smoke_tester_macros::test(session)]
fn benchmark_memory(dut: &DutDefinition, session: &mut Session) -> TestResult {
    if dut.chip.name == "STM32H753ZI" {
        skip_test!("STM32H7 memory read has some unknown issue - maybe cache problems");
    }

    let output = std::env::var_os(OUTPUT_ENV)
        .map(|path| OpenOptions::new().create(true).append(true).open(path))
        .transpose()
        .context("Failed to open the benchmark output file")?;

    let (probe, protocol, speed_khz) = match session.driver_mut() {
        Some(driver) => (
            driver.get_name().to_string(),
            driver.active_protocol().map(|p| p.to_string()),
            driver.speed_khz(),
        ),
        None => ("unknown".to_string(), None, 0),
    };

    let mut bench = Bench {
        output,
        chip: dut.chip.name.clone(),
        probe,
        protocol,
        speed_khz,
    };

    let mut core = session.core(0)?;
    core.reset_and_halt(Duration::from_millis(500))?;

    let Some(ram) = core
        .memory_regions()
        .find_map(MemoryRegion::as_ram_region)
        .cloned()
    else {
        skip_test!("No RAM region found");
    };
    let address = ram.range.start;
    let ram_size = (ram.range.end - ram.range.start) as usize & !3;

    bench.latency(&mut core, address)?;
    for size in BULK_SIZES {
        bench.bulk(&mut core, address, size.min(ram_size))?;
    }

    Ok(())
}
