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

        let drained = buffer.drain(..read).collect::<Vec<_>>();
        log::debug!(
            "Drained {} for {:?}",
            read,
            String::from_utf8_lossy(&drained)
        );

        // TODO: Fix this later on. I am sure there is a better way to handle this in the parser maybe.
        if drained[0] == 0x03 {
            packet_stream
                .unbounded_send(CheckedPacket::from_data(PacketKind::Packet, vec![0x03]))?;
        }

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
