use std::sync::Mutex;

use async_std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};
use futures::channel::mpsc;
use gdb_protocol::packet::CheckedPacket;
use probe_rs::Session;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

const CONNECTION_STRING: &str = "127.0.0.1:1337";

/// This is the main entrypoint which we will call to start the GDB stub.
/// This function is blocking. If you would like to use it concurently to other users of the session,
/// please use a thread.
pub fn run(connection_string: Option<impl Into<String>>, session: &Mutex<Session>) -> Result<()> {
    let connection_string = connection_string
        .map(|cs| cs.into())
        .unwrap_or_else(|| CONNECTION_STRING.to_owned());
    log::info!("GDB stub listening on {}", connection_string);
    task::block_on(accept_loop(connection_string, session))
}

/// This function accepts any incomming connection.
async fn accept_loop(addr: impl ToSocketAddrs, session: &Mutex<Session>) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;

    let mut incoming = listener.incoming();
    while let Some(stream) = incoming.next().await {
        if let Err(e) = handle_connection(stream?, session).await {
            log::error!(
                "An error with the current connection has been encountered. It has been closed."
            );
            log::error!("{:?}", e);
        }
    }
    Ok(())
}

/// Handle a single connection of a client
async fn handle_connection(stream: TcpStream, session: &Mutex<Session>) -> Result<()> {
    let (packet_stream_sender, packet_stream_receiver) = mpsc::unbounded();
    let (tbd_sender, tbd_receiver) = mpsc::unbounded();

    log::info!("Accepted a new connection from: {}", stream.peer_addr()?);

    let inbound_broker_handle = task::spawn(inbound_broker_loop(
        stream,
        tbd_sender,
        packet_stream_receiver,
    ));

    super::worker::worker(tbd_receiver, packet_stream_sender, session).await?;

    inbound_broker_handle.await?;

    Ok(())
}

/// The receiver loop handles any messages that are inbound.
async fn inbound_broker_loop(
    mut stream: TcpStream,
    packet_stream: Sender<CheckedPacket>,
    mut packet_stream_2: Receiver<CheckedPacket>,
) -> Result<()> {
    use futures::future::FutureExt;

    let mut buffer = vec![];
    let mut tmp_buf = [0; 1024];

    loop {
        let mut packet_stream_2 = packet_stream_2.next().fuse();
        let mut read = stream.read(&mut tmp_buf).fuse();

        futures::select! {
            packet = packet_stream_2 => {
                if let Some(packet) = packet {
                    super::writer::writer(packet, &mut stream, &packet_stream, &mut buffer).await?
                }
            },
            n = read => {
                match n {
                    Ok(0) => {
                        log::info!("GDB connection closed.");
                        break Ok(());
                    }
                    Ok(n) => {
                        buffer.extend(&tmp_buf[0..n]);
                        log::info!("Current buf {}", String::from_utf8_lossy(&buffer));
                        super::reader::reader(&mut stream, &packet_stream, &mut buffer).await?
                    },
                    Err(_e) => {

                    }
                }
            }
        }
    }
}
