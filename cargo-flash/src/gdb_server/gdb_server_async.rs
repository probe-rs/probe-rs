#![allow(unused_variables)]

use async_std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};
use futures::channel::mpsc;
use gdb_protocol::packet::CheckedPacket;
use probe_rs::session::Session;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

const CONNECTION_STRING: &str = "127.0.0.1:1337";

/// This is the main entrypoint which we will call to start the GDB stub.
pub fn run(connection_string: Option<impl AsRef<str>>, session: Arc<Mutex<Session>>) -> Result<()> {
    let connection_string = connection_string
        .map(|cs| cs.as_ref().to_owned())
        .unwrap_or(CONNECTION_STRING.to_owned());
    println!("GDB stub listening on {}", connection_string);
    task::block_on(accept_loop(connection_string, session))
}

/// This function accepts any incomming connection.
async fn accept_loop(addr: impl ToSocketAddrs, session: Arc<Mutex<Session>>) -> Result<()> {
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
        let worker = task::spawn(super::worker::worker(
            tbd_receiver,
            packet_stream_sender,
            session.clone(),
        ));
        println!("Accepted a new connection from: {}", stream.peer_addr()?);
        // outbound_broker_handle.await?;
        inbound_broker_handle.await?;
        worker.await?;
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
    use futures::future::FutureExt;

    let mut buffer = vec![];
    let mut tmp_buf = [0; 1024];
    // let mut glob = vec![];

    loop {
        // async_std::io::timeout(std::time::Duration::from_millis(50),
        let mut packet_stream_2 = packet_stream_2.next().fuse();
        let mut s = &*stream;
        let mut read = s.read(&mut tmp_buf).fuse();
        // let reader = crate::reader::reader(stream.clone(), packet_stream.clone(), &mut buffer);

        let t = std::time::Instant::now();
        futures::select! {
            packet = packet_stream_2 => {
                println!("WRITE RACE WIN");
                if let Some(packet) = packet {
                    super::writer::writer(packet, stream.clone(), packet_stream.clone(), &mut buffer).await?
                }
            },
            n = read => {
                println!("READ RACE WIN {:?}, {:?}", t.elapsed(), n);
                if let Ok(n) = n {
                    if n > 0 {
                        buffer.extend(&tmp_buf[0..n]);
                        // glob.extend(&tmp_buf[0..n]);
                    }
                    log::info!("Current buf {}", String::from_utf8_lossy(&buffer));
                    super::reader::reader(stream.clone(), packet_stream.clone(), &mut buffer).await?
                }
            }
        }
    }
}
