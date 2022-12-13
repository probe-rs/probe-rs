//! Provides ITM tracing capabilities.

use super::{CoreOptions, ProbeOptions};
use probe_rs::architecture::arm::component::TraceSink;

/// Trace the application using ITM.
///
/// # Args
/// * `shared_options` - Specifies information about which core to trace.
/// * `common` - Specifies information about the probe to use for tracing.
/// * `sink` - Specifies the destination for trace data.
/// * `duration` - Specifies the duration to trace for.
/// * `output_file` - An optionally specified filename to write ITM binary data into.
pub(crate) fn itm_trace(
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
