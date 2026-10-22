//! MCDS trace recording on AURIX emulation devices.

use std::{io::Write as _, path::PathBuf, time::Duration};

use anyhow::Context as _;
use probe_rs::{config::Registry, probe::list::Lister};
use probe_rs_infineon::mcds::{self, McdsConfig, RecordOptions, Timestamp};
use time::{OffsetDateTime, UtcOffset};

use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(subcommand)]
    action: Action,
}

#[derive(clap::Subcommand)]
enum Action {
    /// Record a trace and save it as an `.mcds` file.
    ///
    /// The MCDS configuration comes from the Infineon MCDS configurator: an `.mcds` file (its
    /// START_OF_MCDS_CONFIG section) or a list of `ADDRESS VALUE` lines. Decode the recording
    /// with the Infineon MCDS tools.
    Record(Record),
}

#[derive(clap::Parser)]
struct Record {
    #[clap(flatten)]
    common: ProbeOptions,

    /// The MCDS configuration.
    #[arg(long)]
    config: PathBuf,

    /// The configuration that returns the MCDS to idle after the recording.
    #[arg(long)]
    cleanup: Option<PathBuf>,

    /// The `.mcds` file to write.
    #[arg(long, short)]
    out: PathBuf,

    /// How long to record, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    duration_ms: u64,

    /// Reset and halt the device first. The configuration must then end with the write that
    /// starts CPU0 (a configuration for "after HAR").
    #[arg(long)]
    reset: bool,
}

impl Cmd {
    pub fn run(
        self,
        registry: &mut Registry,
        lister: &Lister,
        utc_offset: UtcOffset,
    ) -> anyhow::Result<()> {
        match self.action {
            Action::Record(record) => record.run(registry, lister, utc_offset),
        }
    }
}

fn read_config(path: &PathBuf) -> anyhow::Result<McdsConfig> {
    let text = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    McdsConfig::parse(&text).with_context(|| format!("in {}", path.display()))
}

impl Record {
    fn run(
        self,
        registry: &mut Registry,
        lister: &Lister,
        utc_offset: UtcOffset,
    ) -> anyhow::Result<()> {
        let config = read_config(&self.config)?;
        let cleanup = self.cleanup.as_ref().map(read_config).transpose()?;
        let options = RecordOptions {
            reset: self.reset,
            duration: Duration::from_millis(self.duration_ms),
            cleanup,
        };

        let (mut session, _probe_options) = self.common.simple_attach(registry, lister)?;
        let bus = session
            .get_tricore_bus()
            .context("MCDS recording needs a TriCore probe (Infineon TAS or an MCD library)")?;
        let started_at = || {
            let now = OffsetDateTime::now_utc().to_offset(utc_offset);
            Timestamp {
                year: now.year(),
                month: now.month() as u8,
                day: now.day(),
                hour: now.hour(),
                minute: now.minute(),
                second: now.second(),
            }
        };
        let recording = mcds::record(bus, &config, &options, started_at, |progress| {
            eprint!(
                "\rRecording: {:>6} ms, {:>5} KiB",
                progress.elapsed.as_millis(),
                progress.trace_kb
            );
        });
        eprintln!();
        let recording = recording?;

        let mut file = std::fs::File::create(&self.out)
            .with_context(|| format!("creating {}", self.out.display()))?;
        recording.write_mcds(&mut file, concat!("probe-rs ", env!("CARGO_PKG_VERSION")))?;
        file.flush()?;
        println!(
            "Recorded {} KiB in {} ms from {} into {}",
            recording.trace.len() / 1024,
            recording.elapsed.as_millis(),
            recording.device_name,
            self.out.display()
        );
        Ok(())
    }
}
