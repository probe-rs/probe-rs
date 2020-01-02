use async_std::{
    prelude::*,
};
use futures::{channel::mpsc};
use gdb_protocol::{
    packet::{CheckedPacket, Kind as PacketKind},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

pub async fn worker(
    mut input_stream: Receiver<CheckedPacket>,
    output_stream: Sender<CheckedPacket>,
) -> Result<()> {
    while let Some(packet) = input_stream.next().await {
        log::warn!("WORKING {}", String::from_utf8_lossy(&packet.data));
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
                log::debug!("{:x?}", std::str::from_utf8(&response.data).unwrap());
                log::debug!("-----------------------------------------------");
                output_stream.unbounded_send(response)?;
            };
        }
    }
    Ok(())
}