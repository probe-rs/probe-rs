//! Probe throughput benchmarks.
//!
//! Without a benchmark output file, each operation runs once as a test.

use std::time::{Duration, Instant};

use linkme::distributed_slice;
use probe_rs::{Architecture, Core, MemoryInterface, Session, config::MemoryRegion};

use crate::{
    SESSION_TESTS, TestResult,
    bench::{Improves, Recorder},
    dut_definition::DutDefinition,
    skip_test,
};

/// Minimum total time of the samples of one measurement.
const SAMPLE_TIME: Duration = Duration::from_millis(200);
const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 1000;

/// Bulk transfer sizes in bytes.
const BULK_SIZES: [usize; 2] = [64, 16 * 1024];

/// Run `op` once, then again until enough samples are collected if `recorder` records.
///
/// `op` returns the time of the part to measure.
fn sample(
    recorder: &Recorder,
    mut op: impl FnMut() -> anyhow::Result<Duration>,
) -> anyhow::Result<Vec<f64>> {
    op()?;

    let mut samples = Vec::new();
    if !recorder.is_enabled() {
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

/// Measure the time of single accesses in microseconds.
fn latency(recorder: &mut Recorder, core: &mut Core, address: u64) -> anyhow::Result<()> {
    let samples = sample(recorder, || {
        let start = Instant::now();
        core.read_word_32(address)?;
        Ok(start.elapsed())
    })?;
    recorder.record("read_word_32", 4, "us", Improves::Down, to_micros(samples))?;

    // The Xtensa driver caches register values, so the reads do not access the probe.
    if core.architecture() == Architecture::Xtensa {
        return Ok(());
    }

    let pc = core.program_counter();
    let samples = sample(recorder, || {
        let start = Instant::now();
        core.read_core_reg::<u64>(pc)?;
        Ok(start.elapsed())
    })?;
    recorder.record("read_core_reg", 0, "us", Improves::Down, to_micros(samples))
}

/// Measure the throughput of block accesses in bytes per second.
fn bulk(recorder: &mut Recorder, core: &mut Core, address: u64, size: usize) -> anyhow::Result<()> {
    let mut data = vec![0; size / 4];
    let mut readback = vec![0; size / 4];
    let mut seed = 0;

    let samples = sample(recorder, || {
        seed += 1;
        fill(&mut data, seed);

        let start = Instant::now();
        core.write_32(address, &data)?;
        core.flush()?;
        Ok(start.elapsed())
    })?;
    recorder.record(
        "write_32",
        size as u64,
        "B/s",
        Improves::Up,
        to_throughput(samples, size),
    )?;

    // Each read also verifies the last write.
    let samples = sample(recorder, || {
        readback.fill(0);

        let start = Instant::now();
        core.read_32(address, &mut readback)?;
        let time = start.elapsed();

        verify(&data, &readback, address)?;
        Ok(time)
    })?;
    recorder.record(
        "read_32",
        size as u64,
        "B/s",
        Improves::Up,
        to_throughput(samples, size),
    )
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

    let mut recorder = Recorder::new(dut, session)?;

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

    latency(&mut recorder, &mut core, address)?;
    for size in BULK_SIZES {
        bulk(&mut recorder, &mut core, address, size.min(ram_size))?;
    }

    Ok(())
}
