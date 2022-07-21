//! Provides ITM tracing capabilities.

use super::{CoreOptions, ProbeOptions};
use probe_rs::architecture::arm::component::TraceSink;
use std::io::Write;

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
    output_file: Option<String>,
) -> anyhow::Result<()> {
    let mut session = common.simple_attach()?;

    session.setup_tracing(shared_options.core, sink)?;

    let mut decoder = itm_decode::Decoder::new(itm_decode::DecoderOptions::default());

    // If the user specified an output file, create it and open it for writing now.
    let mut output = if let Some(destination) = output_file {
        Some(
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(destination)?,
        )
    } else {
        None
    };

    let start = std::time::Instant::now();

    while start.elapsed() < duration {
        let itm_data = session.read_trace_data()?;

        if itm_data.is_empty() {
            log::info!("No trace data read, exitting");
            break;
        }

        // Write the raw ITM data to the output file if one was opened.
        if let Some(ref mut output) = output {
            output.write_all(&itm_data)?;
        }

        // Decode and print the ITM data for display.
        decoder.push(&itm_data);
        while let Some(packet) = decoder.pull_with_timestamp() {
            println!("{packet:?}");
        }
    }

    Ok(())
}
