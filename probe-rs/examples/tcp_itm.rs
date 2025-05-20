//! This example demonstrates how to use the ITM decoder to decode ITM packets from a target.

use probe_rs::architecture::arm::{component::TraceSink, swo::SwoConfig};
use probe_rs::{Error, Permissions, probe::list::Lister};

use itm::{Decoder, DecoderOptions, TracePacket};

fn main() -> Result<(), Error> {
    async_io::block_on(async move {
        env_logger::init();

        let lister = Lister::new();

        // Get a list of all available debug probes.
        let probes = lister.list_all().await;

        // Use the first probe found.
        let probe = probes[0].open()?;

        // Attach to a chip.
        let mut session = probe.attach("stm32f407", Permissions::default())?;

        // Create a new SwoConfig with a system clock frequency of 16MHz
        let cfg = SwoConfig::new(16_000_000)
            .set_baud(2_000_000)
            .set_continuous_formatting(false);

        session.setup_tracing(0, TraceSink::Swo(cfg))?;

        let mut timestamp: f64 = 0.0;

        let decoder = Decoder::new(session.swo_reader()?, DecoderOptions { ignore_eof: true });

        let mut stimuli = vec![String::new(); 32];

        println!("Starting SWO trace ...");

        for packet in decoder.singles() {
            match packet {
                Ok(TracePacket::LocalTimestamp1 { ts, data_relation }) => {
                    tracing::debug!(
                        "Timestamp packet: data_relation={:?} ts={}",
                        data_relation,
                        ts
                    );
                    let mut time_delta: f64 = ts as f64;
                    // Divide by core clock frequency to go from ticks to seconds.
                    time_delta /= 16_000_000.0;
                    timestamp += time_delta;
                }
                // TracePacket::DwtData { id, payload } => {
                //     tracing::warn!("Dwt: id={} payload={:?}", id, payload);

                //     if id == 17 {
                //         let value: i32 = payload.pread(0).unwrap();
                //         tracing::trace!("VAL={}", value);
                //         // client.send_sample("a", timestamp, value as f64).unwrap();
                //     }
                // }
                Ok(TracePacket::Instrumentation { port, payload }) => {
                    let id = port as usize;
                    // First decode the string data from the stimuli.
                    stimuli[id].push_str(&String::from_utf8_lossy(&payload));
                    // Then collect all the lines we have gotten so far.
                    let data = stimuli[id].clone();
                    let mut lines: Vec<_> = data.lines().collect();

                    // If there is at least one char in the total of all received chars, look at the last one.
                    let last_char = stimuli[id].chars().last();
                    if let Some(last_char) = last_char {
                        // If the last one is not a newline (this is always the last one for Windows (\r\n) as well as Linux (\n)),
                        // we keep the last line as it was not fully received yet.
                        if last_char != '\n' {
                            // Get the last line and keep it if there is even one.
                            if let Some(last_line) = lines.pop() {
                                stimuli[id] = last_line.to_string();
                            }
                        } else {
                            stimuli[id] = String::new();
                        }
                    }

                    // Finally print all due lines!
                    for line in lines {
                        println!("{id}> {line}");
                    }
                }
                _ => {
                    tracing::warn!("Trace packet: {:?}", packet);
                }
            }
            tracing::debug!("{}", timestamp);
        }

        Ok(())
    })
}
