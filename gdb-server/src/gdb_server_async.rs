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
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

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
        let acks_due = Arc::new(AtomicUsize::new(0));
        let (tbd_sender, tbd_receiver) = mpsc::unbounded();
        let stream = Arc::new(stream?);

        // let outbound_broker_handle = task::spawn(outbound_broker_loop(
        //     Arc::clone(&stream),
        //     packet_stream_receiver,
        //     Arc::clone(&acks_due),
        // ));
        let inbound_broker_handle = task::spawn(inbound_broker_loop(
            Arc::clone(&stream),
            tbd_sender,
            packet_stream_receiver,
            acks_due,
        ));
        let worker = task::spawn(crate::worker::worker(
            tbd_receiver,
            packet_stream_sender,
        ));
        println!("Accepted a new connection from: {}", stream.peer_addr()?);
        // outbound_broker_handle.await?;
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
    acks_due: Arc<AtomicUsize>,
) -> Result<()> {
    while let Some(packet) = packet_stream.next().await {
        if packet.is_valid() {
            encode(&packet, &*stream).await?;
            (&*stream).flush().await?;
            println!("Request ACK for {}", String::from_utf8_lossy(&packet.data));
            acks_due.fetch_add(1, Ordering::SeqCst);
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
    mut packet_stream_2: Receiver<CheckedPacket>,
    acks_due: Arc<AtomicUsize>,
) -> Result<()> {
    use ReceiverState::*;
    let mut receiver_state = AwaitPacket;
    let mut parser = Parser::default();

    let mut buffer = vec![];
    let mut tmp_buf = [0; 128];
    // let mut glob = vec![];

    loop {
        while let Some(packet) = packet_stream_2.next().await {
            if packet.is_valid() {
                encode(&packet, &*stream).await?;
                (&*stream).flush().await?;
                println!("Request ACK for {}", String::from_utf8_lossy(&packet.data));
                acks_due.fetch_add(1, Ordering::SeqCst);
                loop {
                    let n = (&*stream).read(&mut tmp_buf).await?;
                    if n > 0 {
                        buffer.extend(&tmp_buf[0..n]);
                        // glob.extend(&tmp_buf[0..n]);
                        log::info!("Current buf {}", String::from_utf8_lossy(&buffer));
                    }
                    let mut i = 0;
                    for byte in buffer.iter() {
                        match byte {
                            b'+' => {
                                log::debug!("Ack received.");
                                acks_due.fetch_sub(1, Ordering::SeqCst);
                                i += 1;
                                break;
                            }
                            b'-' => {
                                log::debug!("Nack received. Retrying.");
                                i += 1;
                                continue;
                            }
                            // This should never happen.
                            // And if it does, GDB fucked up, so we might as well stop.
                            _ => break,
                        }
                    }
                    buffer.drain(..i);
                }
            } else {
                log::warn!("Broken packet! It will not be sent.");
            }
        }

        log::trace!("Working Inbound ...");
        let n = (&*stream).read(&mut tmp_buf).await?;
        if n > 0 {
            buffer.extend(&tmp_buf[0..n]);
            // glob.extend(&tmp_buf[0..n]);
            log::info!("Current buf {}", String::from_utf8_lossy(&buffer));
        }
        if acks_due.load(Ordering::SeqCst) > 0 {
            log::debug!("Received new request for ack.");
            receiver_state = AwaitAck;
        }
        // println!("{}", String::from_utf8_lossy(&glob));

        // continue;

        match receiver_state {
            AwaitPacket => {
                log::trace!("Awaiting packet");
                let (read, packet) = parser.feed(&buffer)?;
                buffer.drain(..read);
                println!("Drained {} for {:?}", read, packet);

                if let Some(packet) = packet {
                    match packet.kind {
                        PacketKind::Packet => match packet.check() {
                            Some(checked) => {
                                println!("Sending ACK");
                                (&*stream).write_all(&[b'+']).await?;
                                packet_stream.unbounded_send(checked)?;
                            }
                            None => {
                                println!("Sending nACK");
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
                log::trace!("Awaiting ack");
                let mut i = 0;
                for byte in buffer.iter() {
                    match byte {
                        b'+' => {
                            log::debug!("Ack received.");
                            acks_due.fetch_sub(1, Ordering::SeqCst);
                            i += 1;
                            break;
                        }
                        b'-' => {
                            log::debug!("Nack received. Retrying.");
                            i += 1;
                            continue;
                        }
                        // This should never happen.
                        // And if it does, GDB fucked up, so we might as well stop.
                        _ => break,
                    }
                }
                buffer.drain(..i);
            }
        }
    }
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
