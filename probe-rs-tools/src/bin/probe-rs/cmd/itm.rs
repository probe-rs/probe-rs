//! Provides ITM tracing capabilities.

use std::time::Duration;
use web_time::Instant;

use probe_rs::architecture::arm::{component::TraceSink, swo::SwoConfig};
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;

use crate::CoreOptions;
use crate::util::common_options::ProbeOptions;

#[derive(clap::Subcommand)]
pub(crate) enum ItmSource {
    /// Direct ITM data to Embedded Trace Buffer/FIFO (ETB/ETF) for extraction.
    ///
    /// Note: Not all targets support ETF.
    ///
    /// The tracing infrastructure allows trace data to be generated on the SWO
    /// output, which is a UART output to the debug probe. Because of the nature of
    /// this output, the throughput is inherently limited. Additionally, there is
    /// very little buffering between ITM packet generation and SWO output, so even
    /// a small amount of trace data generated in a short time interval can result
    /// in the trace data overflowing in the SWO data path.
    ///
    /// Alternatively there is a wide, source synchronous and fast output through
    /// the TPIU. But that requires the availability of certain pins and a logic
    /// analyzer or a capable probe to capture the data.
    ///
    /// To work around issues with buffering and throughput of the SWO output and to
    /// avoid the need for special hardware this program provides a mechanism to
    /// instead capture DWT/ITM trace data within the Embedded Trace Buffer/FIFO
    /// (ETB/ETF). The ETF is a 4 KiB (usually) FIFO in SRAM that can be used to
    /// buffer data before draining the trace data to an external source. The ETF
    /// supports draining data through the debug registers.
    ///
    /// This program uses the ETF in "software" mode with no external tracing
    /// utilities required. Instead, the ETF is used to buffer up a trace which is
    /// then read out from the device via the debug probe.
    #[clap(name = "memory")]
    TraceMemory {
        /// The core clock frequency in Hz.
        coreclk: u32,
    },

    /// Direct ITM traffic out the TRACESWO pin for reception by the probe.
    #[clap(name = "swo")]
    Swo {
        /// The trace duration in ms.
        duration: u64,

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

    #[clap(subcommand)]
    source: ItmSource,
}

impl Cmd {
    pub async fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(registry, lister)?;

        match self.source {
            ItmSource::TraceMemory { coreclk } => {
                session
                    .setup_tracing(self.shared.core, TraceSink::TraceMemory)
                    .await?;

                let trace = session.read_trace_data().await?;
                let decoder =
                    itm::Decoder::new(trace.as_slice(), itm::DecoderOptions { ignore_eof: false });

                let timestamp_cfg = itm::TimestampsConfiguration {
                    clock_frequency: coreclk,
                    lts_prescaler: itm::LocalTimestampOptions::Enabled,
                    expect_malformed: false,
                };
                for packet in decoder.timestamps(timestamp_cfg) {
                    println!("{packet:?}");
                }
            }

            ItmSource::Swo {
                duration,
                clk,
                baud,
            } => {
                session
                    .setup_tracing(
                        self.shared.core,
                        TraceSink::Swo(SwoConfig::new(clk).set_baud(baud)),
                    )
                    .await?;

                let decoder = itm::Decoder::new(
                    session.swo_reader()?,
                    itm::DecoderOptions { ignore_eof: true },
                );

                let start = Instant::now();
                let stop = Duration::from_millis(duration);
                for packet in decoder.singles() {
                    println!("{packet:?}");
                    if start.elapsed() > stop {
                        return Ok(());
                    }
                }
            }
        };
        Ok(())
    }
}
