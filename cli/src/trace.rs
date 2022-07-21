//! Provides ITM tracing capabilities.

use super::{CoreOptions, ProbeOptions};
use probe_rs::architecture::arm::component::TraceSink;

pub(crate) fn itm_trace(
    shared_options: &CoreOptions,
    common: &ProbeOptions,
    sink: TraceSink,
    duration: std::time::Duration,
) -> anyhow::Result<()> {
    let mut session = common.simple_attach()?;

    session.setup_tracing(shared_options.core, sink)?;

    let mut decoder = itm_decode::Decoder::new(itm_decode::DecoderOptions::default());

    let start = std::time::Instant::now();

    while start.elapsed() < duration {
        let itm_data = session.read_trace_data()?;
        if itm_data.is_empty() {
            log::info!("No trace data read, exitting");
            break;
        }

        decoder.push(&itm_data);

        while let Some(packet) = decoder.pull_with_timestamp() {
            println!("{packet:?}");
        }
    }

    Ok(())
}
