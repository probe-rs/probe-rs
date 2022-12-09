use probe_rs::architecture::arm::{component::TraceSink, swo::SwoConfig};
use probe_rs::{Error, Permissions};

use itm::{Decoder, DecoderOptions, TracePacket};

use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{sleep, spawn, JoinHandle};
use std::{any::Any, io::prelude::*};

fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    use probe_rs::Probe;

    // Get a list of all available debug probes.
    let probes = Probe::list_all();

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
                    println!("{}> {}", id, line);
                }
            }
            _ => {
                tracing::warn!("Trace packet: {:?}", packet);
            }
        }
        tracing::debug!("{}", timestamp);
    }

    Ok(())
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
                        tracing::error!("Writing to a tcp socket experienced an error: {:?}", err)
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

pub trait SwoPublisher<E> {
    /// Starts the `SwoPublisher`.
    /// This should never block and run the `Updater` asynchronously.
    fn start<
        I: Serialize + Send + Sync + 'static,
        O: Deserialize<'static> + Send + Sync + 'static,
    >(
        &mut self,
    ) -> UpdaterChannel<I, O>;
    /// Stops the `SwoPublisher` if currently running.
    /// Returns `Ok` if everything went smooth during the run of the `SwoPublisher`.
    /// Returns `Err` if something went wrong during the run of the `SwoPublisher`.
    fn stop(&mut self) -> Result<(), E>;
}

/// A complete channel to an updater.
/// Rx and tx naming is done from the user view of the channel, not the `Updater` view.
pub struct UpdaterChannel<
    I: Serialize + Send + Sync + 'static,
    O: Deserialize<'static> + Send + Sync + 'static,
> {
    /// The rx where the user reads data from.
    rx: Receiver<O>,
    /// The tx where the user sends data to.
    tx: Sender<I>,
}

impl<I: Serialize + Send + Sync + 'static, O: Deserialize<'static> + Send + Sync + 'static>
    UpdaterChannel<I, O>
{
    /// Creates a new `UpdaterChannel` where crossover is done internally.
    /// The argument naming is done from the `Updater`s view. Where as the member naming is done from a user point of view.
    pub fn new(rx: Sender<I>, tx: Receiver<O>) -> Self {
        Self { rx: tx, tx: rx }
    }

    /// Returns the rx end of the channel.
    pub fn rx(&mut self) -> &mut Receiver<O> {
        &mut self.rx
    }

    /// Returns the tx end of the channel.
    pub fn tx(&mut self) -> &mut Sender<I> {
        &mut self.tx
    }
}

impl SwoPublisher<Box<dyn Any + Send>> for TcpPublisher {
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

        tracing::info!("Opening websocket on '{}'", self.connection_string);
        let server = TcpListener::bind(&self.connection_string).unwrap();
        server.set_nonblocking(true).unwrap();

        self.thread_handle = Some((
            spawn(move || {
                let mut incoming = server.incoming();
                loop {
                    // If a halt was requested, cease operations.
                    if halt_rx.try_recv().is_ok() {
                        return;
                    }

                    // Handle new incomming connections.
                    match incoming.next() {
                        Some(Ok(stream)) => {
                            // Assume we always get a peer addr, so this unwrap is fine.
                            let addr = stream.peer_addr().unwrap();

                            // Make sure we operate in nonblocking mode.
                            // Is is required so read does not block forever.
                            stream.set_nonblocking(true).unwrap();
                            tracing::info!("Accepted a new websocket connection from {}", addr);
                            sockets.push((stream, addr));
                        }
                        Some(Err(err)) => {
                            if err.kind() == std::io::ErrorKind::WouldBlock {
                            } else {
                                tracing::error!(
                                    "Connecting to a websocket experienced an error: {:?}",
                                    err
                                )
                            }
                        }
                        None => {
                            tracing::error!("The TCP listener iterator was exhausted. Shutting down websocket listener.");
                            return;
                        }
                    }

                    // Send at max one pending message to each socket.
                    if let Ok(update) = inbound.try_recv() {
                        Self::write_to_all_sockets(
                            &mut sockets,
                            serde_json::to_string(&update).unwrap(),
                        );
                    }

                    // Pause the current thread to not use CPU for no reason.
                    sleep(std::time::Duration::from_micros(100));
                }
            }),
            halt_tx,
        ));

        UpdaterChannel::new(rx, tx)
    }

    fn stop(&mut self) -> Result<(), Box<dyn Any + Send>> {
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
                tracing::error!("An error occurred during thread execution: {:?}", err);
                Err(err)
            }
            _ => Ok(()),
        }
    }
}
