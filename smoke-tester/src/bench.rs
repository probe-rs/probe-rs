//! Benchmark result output.
//!
//! Set `SMOKE_TESTER_BENCH_OUT` to a file path to record results. The results are appended to the
//! file as JSON lines.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Write,
    time::{Duration, Instant},
};

use anyhow::Context;
use probe_rs::{
    Session,
    flashing::{ProgressEvent, ProgressOperation},
};
use serde::Serialize;

use crate::dut_definition::DutDefinition;

const OUTPUT_ENV: &str = "SMOKE_TESTER_BENCH_OUT";

/// The direction in which a metric gets better.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Improves {
    Up,
    Down,
}

#[derive(Serialize)]
struct Record<'a> {
    chip: &'a str,
    probe: &'a str,
    protocol: Option<&'a str>,
    speed_khz: u32,
    metric: &'a str,
    size: u64,
    unit: &'a str,
    improves: Improves,
    median: f64,
    p10: f64,
    p90: f64,
    samples: usize,
}

/// Writes benchmark results for one DUT.
pub struct Recorder {
    output: Option<File>,
    chip: String,
    probe: String,
    protocol: Option<String>,
    speed_khz: u32,
}

impl Recorder {
    pub fn new(dut: &DutDefinition, session: &mut Session) -> anyhow::Result<Self> {
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

        Ok(Self {
            output,
            chip: dut.chip.name.clone(),
            probe,
            protocol,
            speed_khz,
        })
    }

    /// Returns `true` if results are recorded.
    pub fn is_enabled(&self) -> bool {
        self.output.is_some()
    }

    /// Write the statistics of `values` to the output file.
    pub fn record(
        &mut self,
        metric: &str,
        size: u64,
        unit: &str,
        improves: Improves,
        mut values: Vec<f64>,
    ) -> anyhow::Result<()> {
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        anyhow::ensure!(!values.is_empty(), "No samples for {metric}");

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
}

#[derive(Default)]
struct Phase {
    bytes: u64,
    time: Duration,
    started: Option<Instant>,
}

/// Measures the throughput of each flash operation from the flash progress events.
#[derive(Default)]
pub struct FlashTimer {
    phases: BTreeMap<&'static str, Phase>,
}

impl FlashTimer {
    pub fn event(&mut self, event: &ProgressEvent) {
        match event {
            ProgressEvent::AddProgressBar {
                operation,
                total: Some(total),
            } => self.phase(*operation).bytes += total,
            ProgressEvent::Started(operation) => {
                self.phase(*operation).started = Some(Instant::now());
            }
            ProgressEvent::Finished(operation) => {
                let phase = self.phase(*operation);
                if let Some(started) = phase.started.take() {
                    phase.time += started.elapsed();
                }
            }
            _ => {}
        }
    }

    fn phase(&mut self, operation: ProgressOperation) -> &mut Phase {
        let metric = match operation {
            ProgressOperation::Fill => "flash_fill",
            ProgressOperation::Erase => "flash_erase",
            ProgressOperation::Program => "flash_program",
            ProgressOperation::Verify => "flash_verify",
            ProgressOperation::Ram => "ram_load",
        };
        self.phases.entry(metric).or_default()
    }

    /// Record the throughput of each finished operation with a known size, in bytes per second.
    pub fn record(self, recorder: &mut Recorder) -> anyhow::Result<()> {
        for (metric, phase) in self.phases {
            if phase.bytes == 0 || phase.time.is_zero() {
                continue;
            }
            let throughput = phase.bytes as f64 / phase.time.as_secs_f64();
            recorder.record(metric, phase.bytes, "B/s", Improves::Up, vec![throughput])?;
        }
        Ok(())
    }
}
