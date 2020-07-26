use probe_rs::architecture::arm::swo::{
    Decoder, SwoConfig, SwoPublisher, TimeStamp, TracePacket, TracePackets, UpdaterChannel,
};
use probe_rs::Error;
use serde::{Deserialize, Serialize};
use std::io::prelude::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::thread::{sleep, spawn, JoinHandle};

fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    use probe_rs::Probe;

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("stm32f407")?;

    // Create a new SwoConfig with a system clock frequency of 16MHz
    let cfg = SwoConfig::new(16_000_000)
        .set_baud(2_000_000)
        .set_continuous_formatting(false);

    session.setup_swv(&cfg)?;

    let mut timestamp: f64 = 0.0;

    let mut decoder = Decoder::new();

    let mut stimuli = vec![String::new(); 32];

    println!("Starting SWO trace ...");

    loop {
        let bytes = session.read_swo()?;

        decoder.feed(bytes);
        while let Some(TracePackets {
            packets,
            timestamp: TimeStamp { tc, ts },
        }) = decoder.pull()
        {
            log::debug!("Timestamp packet: tc={:?} ts={}", tc, ts);
            let mut time_delta: f64 = ts as f64;
            // Divide by core clock frequency to go from ticks to seconds.
            time_delta /= 16_000_000.0;
            timestamp += time_delta;

            for packet in packets {
                match packet {
                    // TracePacket::DwtData { id, payload } => {
                    //     log::warn!("Dwt: id={} payload={:?}", id, payload);

                    //     if id == 17 {
                    //         let value: i32 = payload.pread(0).unwrap();
                    //         log::trace!("VAL={}", value);
                    //         // client.send_sample("a", timestamp, value as f64).unwrap();
                    //     }
                    // }
                    TracePacket::ItmData { id, payload } => {
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
                            println!("{}> {}", id, line);
                        }
                    }
                    _ => {
                        log::warn!("Trace packet: {:?}", packet);
                    }
                }
            }
            log::debug!("{}", timestamp);
        }
    }
}

pub struct TcpPublisher {
    connection_string: String,
    thread_handle: Option<(JoinHandle<()>, Sender<()>)>,
}

impl TcpPublisher {
    pub fn new(connection_string: impl Into<String>) -> Self {
        Self {
            connection_string: connection_string.into(),
            thread_handle: None,
        }
    }

    /// Writes a message to all connected websockets and removes websockets that are no longer connected.
    fn write_to_all_sockets(sockets: &mut Vec<(TcpStream, SocketAddr)>, message: impl AsRef<str>) {
        let mut to_remove = vec![];
        for (i, (socket, _addr)) in sockets.iter_mut().enumerate() {
            match socket.write(message.as_ref().as_bytes()) {
                Ok(_) => (),
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                    } else {
                        to_remove.push(i);
                        log::error!("Writing to a tcp socket experienced an error: {:?}", err)
                    }
                }
            }
        }

        // Remove all closed websockets.
        for i in to_remove.into_iter().rev() {
            sockets.swap_remove(i);
        }
    }
}

impl SwoPublisher for TcpPublisher {
    fn start<
        I: Serialize + Send + Sync + 'static,
        O: Deserialize<'static> + Send + Sync + 'static,
    >(
        &mut self,
    ) -> UpdaterChannel<I, O> {
        let mut sockets = Vec::new();

        let (rx, inbound) = channel::<I>();
        let (_outbound, tx) = channel::<O>();
        let (halt_tx, halt_rx) = channel::<()>();

        log::info!("Opening websocket on '{}'", self.connection_string);
        let server = TcpListener::bind(&self.connection_string).unwrap();
        server.set_nonblocking(true).unwrap();

        self.thread_handle = Some((
            spawn(move || {
                let mut incoming = server.incoming();
                loop {
                    // If a halt was requested, cease operations.
                    if halt_rx.try_recv().is_ok() {
                        return ();
                    }

                    // Handle new incomming connections.
                    match incoming.next() {
                        Some(Ok(stream)) => {
                            // Assume we always get a peer addr, so this unwrap is fine.
                            let addr = stream.peer_addr().unwrap();

                            // Make sure we operate in nonblocking mode.
                            // Is is required so read does not block forever.
                            stream.set_nonblocking(true).unwrap();
                            log::info!("Accepted a new websocket connection from {}", addr);
                            sockets.push((stream, addr));
                        }
                        Some(Err(err)) => {
                            if err.kind() == std::io::ErrorKind::WouldBlock {
                            } else {
                                log::error!(
                                    "Connecting to a websocket experienced an error: {:?}",
                                    err
                                )
                            }
                        }
                        None => {
                            log::error!("The TCP listener iterator was exhausted. Shutting down websocket listener.");
                            return ();
                        }
                    }

                    // Send at max one pending message to each socket.
                    match inbound.try_recv() {
                        Ok(update) => {
                            Self::write_to_all_sockets(
                                &mut sockets,
                                serde_json::to_string(&update).unwrap(),
                            );
                        }
                        _ => (),
                    }

                    // Pause the current thread to not use CPU for no reason.
                    sleep(std::time::Duration::from_micros(100));
                }
            }),
            halt_tx,
        ));

        UpdaterChannel::new(rx, tx)
    }

    fn stop(&mut self) -> Result<(), ()> {
        let thread_handle = self.thread_handle.take();
        match thread_handle.map(|h| {
            // If we have a running thread, send the request to stop it and then wait for a join.
            // If this unwrap fails the thread has already been destroyed.
            // This cannot be assumed under normal operation conditions. Even with normal fault handling this should never happen.
            // So this unwarp is fine.
            h.1.send(()).unwrap();
            h.0.join()
        }) {
            Some(Err(err)) => {
                log::error!("An error occured during thread execution: {:?}", err);
                Err(())
            }
            _ => Ok(()),
        }
    }
}
