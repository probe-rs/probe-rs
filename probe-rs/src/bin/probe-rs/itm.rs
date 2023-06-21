//! Provides ITM tracing capabilities.

use probe_rs::architecture::arm::{component::TraceSink, swo::SwoConfig};

use super::CoreOptions;
use crate::util::{common_options::ProbeOptions, parse_u64};

#[derive(clap::Subcommand)]
pub(crate) enum ItmSource {
    /// Direct ITM data to internal trace memory for extraction.
    /// Note: Not all targets support trace memory.
    #[clap(name = "memory")]
    TraceMemory,

    /// Direct ITM traffic out the TRACESWO pin for reception by the probe.
    #[clap(name = "swo")]
    Swo {
        /// The speed of the clock feeding the TPIU/SWO module in Hz.
        clk: u32,

        /// The desired baud rate of the SWO output.
        baud: u32,
    },
}

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    #[clap(value_parser = parse_u64)]
    duration_ms: u64,

    #[clap(subcommand)]
    source: ItmSource,
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let sink = match self.source {
            ItmSource::TraceMemory => TraceSink::TraceMemory,
            ItmSource::Swo { clk, baud } => TraceSink::Swo(SwoConfig::new(clk).set_baud(baud)),
        };
        itm_trace(
            &self.shared,
            &self.common,
            sink,
            std::time::Duration::from_millis(self.duration_ms),
        )
    }
}

/// Trace the application using ITM.
///
/// # Args
/// * `shared_options` - Specifies information about which core to trace.
/// * `common` - Specifies information about the probe to use for tracing.
/// * `sink` - Specifies the destination for trace data.
/// * `duration` - Specifies the duration to trace for.
/// * `output_file` - An optionally specified filename to write ITM binary data into.
fn itm_trace(
    shared_options: &CoreOptions,
    common: &ProbeOptions,
    sink: TraceSink,
    duration: std::time::Duration,
) -> anyhow::Result<()> {
    let mut session = common.simple_attach()?;

    session.setup_tracing(shared_options.core, sink)?;

    let decoder = itm::Decoder::new(
        session.swo_reader()?,
        itm::DecoderOptions { ignore_eof: true },
    );

    let start = std::time::Instant::now();
    let iter = decoder.singles();

    // Decode and print the ITM data for display.
    for packet in iter {
        if start.elapsed() > duration {
            return Ok(());
        }

        println!("{packet:?}");
    }

    Ok(())
}
