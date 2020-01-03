#![allow(unused_variables)]

use async_std::{net::TcpStream, prelude::*};
use futures::channel::mpsc;
use gdb_protocol::{
    packet::{CheckedPacket, Kind as PacketKind},
    parser::Parser,
};
use std::sync::Arc;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;

pub async fn reader(
    stream: Arc<TcpStream>,
    packet_stream: Sender<CheckedPacket>,
    buffer: &mut Vec<u8>,
) -> Result<()> {
    log::debug!("READ WIN");
    let mut parser = Parser::default();
    log::trace!("Awaiting packet");
    while buffer.len() > 0 {
        let (read, packet) = parser.feed(&buffer)?;

        let drained = buffer.drain(..read);
        log::debug!(
            "Drained {} for {:?}",
            read,
            String::from_utf8_lossy(&drained.collect::<Vec<_>>())
        );

        if let Some(packet) = packet {
            match packet.kind {
                PacketKind::Packet => match packet.check() {
                    Some(checked) => {
                        log::debug!("Sending ACK");
                        (&*stream).write_all(&[b'+']).await?;
                        packet_stream.unbounded_send(checked)?;
                    }
                    None => {
                        log::debug!("Sending nACK");
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
        } else {
            break;
        }
    }
    Ok(())
}
