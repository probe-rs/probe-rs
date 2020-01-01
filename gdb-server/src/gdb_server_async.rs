#![allow(unused_variables)]

use async_std::{
    io::{Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};
use futures::{channel::mpsc};
use gdb_protocol::{
    packet::{CheckedPacket, Kind as PacketKind},
    parser::Parser,
};
use std::sync::Arc;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

const CONNECTION_STRING: &str = "127.0.0.1:1337";

/// This is the main entrypoint which we will call to start the GDB stub.
pub fn run() -> Result<()> {
    println!("Listening on {}", CONNECTION_STRING);
    task::block_on(accept_loop(CONNECTION_STRING))
}

/// This method is a helper to spawn a new thread and await the future on that trait.
/// If an error occurs during execution it will be logged.
fn spawn_and_log_error<F>(future: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    task::spawn(async move {
        if let Err(e) = future.await {
            eprintln!("{}", e)
        }
    })
}

/// This function accepts any incomming connection.
async fn accept_loop(addr: impl ToSocketAddrs) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;

    let mut incoming = listener.incoming();
    while let Some(stream) = incoming.next().await {
        let (packet_stream_sender, packet_stream_receiver) = mpsc::unbounded();
        let (control_stream_sender, control_stream_receiver) = mpsc::unbounded();
        let (tbd_sender, tbd_receiver) = mpsc::unbounded();
        let stream = Arc::new(stream?);

        let outbound_broker_handle = task::spawn(outbound_broker_loop(
            Arc::clone(&stream),
            packet_stream_receiver,
            control_stream_sender,
        ));
        let inbound_broker_handle = task::spawn(inbound_broker_loop(
            Arc::clone(&stream),
            tbd_sender,
            control_stream_receiver,
        ));
        let worker = task::spawn(worker(
            tbd_receiver,
            packet_stream_sender,
        ));
        println!("Accepted a new connection from: {}", stream.peer_addr()?);
        outbound_broker_handle.await?;
        inbound_broker_handle.await?;
        worker.await?;
    }
    Ok(())
}

/// The transmitter loop handles any messages that are outbound.
/// It will take care of delivering any message to GDB reliably.
/// This means that it also handles retransmission and ACKs.
async fn outbound_broker_loop(
    stream: Arc<TcpStream>,
    mut packet_stream: Receiver<CheckedPacket>,
    control_stream: Sender<ReceiverState>,
) -> Result<()> {
    while let Some(packet) = packet_stream.next().await {
        if packet.is_valid() {
            encode(&packet, &*stream).await?;
            (&*stream).flush().await?;
            control_stream.unbounded_send(ReceiverState::AwaitAck)?;
        } else {
            log::warn!("Broken packet! It will not be sent.");
        }
    }
    Ok(())
}

/// The receiver loop handles any messages that are inbound.
async fn inbound_broker_loop(
    stream: Arc<TcpStream>,
    packet_stream: Sender<CheckedPacket>,
    mut control_stream: Receiver<ReceiverState>,
) -> Result<()> {
    use ReceiverState::*;
    let mut receiver_state = AwaitPacket;
    let mut parser = Parser::default();

    let mut buffer = vec![];
    let mut tmp_buf = [0; 128];

    loop {
        log::trace!("Working Inbound ...");
        if let Ok(Some(state)) = control_stream.try_next() {
            log::debug!("Received new request for ack.");
            receiver_state = state;
        }
        let n = (&*stream).read(&mut tmp_buf).await?;
        if n > 0 {
            buffer.extend(&tmp_buf[0..n]);
            log::trace!("Read {} bytes.", n);
        }

        match receiver_state {
            AwaitPacket => {
                log::trace!("Awaiting packet");
                let (read, packet) = parser.feed(&buffer)?;
                buffer.drain(..read);

                if let Some(packet) = packet {
                    match packet.kind {
                        PacketKind::Packet => match packet.check() {
                            Some(checked) => {
                                (&*stream).write_all(&[b'+']).await?;
                                packet_stream.unbounded_send(checked)?;
                            }
                            None => {
                                (&*stream).write_all(&[b'-']).await?;
                            }
                        },
                        // Protocol specifies notifications should not be checked
                        PacketKind::Notification => {
                            if let Some(checked) = packet.check() {
                                packet_stream.unbounded_send(checked)?;
                            }
                        }
                    };
                }
            }
            AwaitAck => {
                log::debug!("Awaiting ack");
                for (i, byte) in buffer.iter().enumerate() {
                    match byte {
                        b'+' => {
                            log::debug!("Ack received.");
                            receiver_state = AwaitPacket;
                            break;
                        }
                        b'-' => {
                            log::debug!("Nack received. Retrying.");
                            continue;
                        }
                        // This should never happen.
                        // And if it does, GDB fucked up, so we might as well stop.
                        _ => break,
                    }
                }
            }
        }
    }
}


async fn worker(
    mut input_stream: Receiver<CheckedPacket>,
    output_stream: Sender<CheckedPacket>,
) -> Result<()> {
    while let Some(packet) = input_stream.next().await {
        if packet.is_valid() {
            let response: Option<String> = if packet.data.starts_with("qSupported".as_bytes()) {
                Some(
                    "PacketSize=2048;swbreak-;hwbreak+;vContSupported+;qXfer:memory-map:read+"
                        .into(),
                )
            } else if packet.data.starts_with("vMustReplyEmpty".as_bytes()) {
                Some("".into())
            } else {
                Some("OK".into())
            };

            if let Some(response) = response {
                let response = CheckedPacket::from_data(PacketKind::Packet, response.into_bytes());

                let mut bytes = Vec::new();
                response.encode(&mut bytes).unwrap();
                println!("{:x?}", std::str::from_utf8(&response.data).unwrap());
                println!("-----------------------------------------------");
                output_stream.unbounded_send(response)?;
            };
        }
    }
    Ok(())
}

pub enum ReceiverState {
    AwaitAck,
    AwaitPacket,
}

pub async fn encode<W>(packet: &CheckedPacket, mut w: W) -> Result<()>
where
    W: Write + Unpin,
{
    w.write_all(&[match packet.kind {
        PacketKind::Notification => b'%',
        PacketKind::Packet => b'$',
    }]).await?;

    let mut remaining: &[u8] = &packet.data;
    while !remaining.is_empty() {
        let escape1 = memchr::memchr3(b'#', b'$', b'}', remaining);
        let escape2 = memchr::memchr(b'*', remaining);

        let escape = std::cmp::min(
            escape1.unwrap_or_else(|| remaining.len()),
            escape2.unwrap_or_else(|| remaining.len()),
        );

        w.write_all(&remaining[..escape]).await?;
        remaining = &remaining[escape..];

        if let Some(&b) = remaining.first() {
            dbg!(b as char);
            // memchr found a character that needs escaping, so let's do that
            w.write_all(&[b'}', b ^ 0x20]).await?;
            remaining = &remaining[1..];
        }
    }

    w.write_all(&[b'#']).await?;
    w.write_all(&packet.checksum).await?;
    Ok(())
}
