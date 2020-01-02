#![allow(unused_variables)]

use async_std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};
use futures::{channel::mpsc};
use gdb_protocol::{
    packet::CheckedPacket,
};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

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
        let mut packet_stream_2 = packet_stream_2.next().fuse();
        let mut s = &*stream;
        let mut read = s.read(&mut tmp_buf).fuse();
        // let reader = crate::reader::reader(stream.clone(), packet_stream.clone(), &mut buffer);
        
        futures::select! {
            packet = packet_stream_2 => {
                println!("WRITE RACE WIN");
                if let Some(packet) = packet {
                    crate::writer::writer(packet, stream.clone(), packet_stream.clone(), &mut buffer).await?
                }
            },
            n = read => {
                println!("READ RACE WIN");
                if let Ok(n) = n {
                    if n > 0 {
                        buffer.extend(&tmp_buf[0..n]);
                        // glob.extend(&tmp_buf[0..n]);
                    }
                    log::info!("Current buf {}", String::from_utf8_lossy(&buffer));
                    crate::reader::reader(stream.clone(), packet_stream.clone(), &mut buffer).await?
                }
            }
        }
    }
}