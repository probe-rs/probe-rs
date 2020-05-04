use async_std::prelude::*;
use futures::channel::mpsc;
use futures::future::FutureExt;
use futures::select;
use gdb_protocol::packet::{CheckedPacket, Kind as PacketKind};
use probe_rs::Core;
use probe_rs::Session;
use std::sync::{Arc, Mutex};

use crate::handlers;

type ServerResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

#[allow(clippy::mutex_atomic)]
pub async fn worker(
    mut input_stream: Receiver<CheckedPacket>,
    output_stream: Sender<CheckedPacket>,
    session: Arc<Mutex<Session>>,
) -> ServerResult<()> {
    let mut session = session.lock().unwrap();
    let mut core = session.attach_to_core(0).unwrap();
    let mut awaits_halt = false;

    loop {
        select! {
            potential_packet = input_stream.next().fuse() => {
                if let Some(packet) = potential_packet {
                    log::warn!("WORKING {}", String::from_utf8_lossy(&packet.data));
                    if handler(&mut core, output_stream.clone(), &mut awaits_halt, packet).await? {
                        break;
                    }
                } else {
                    break
                }
            },
            _ = await_halt(&mut core, output_stream.clone(), awaits_halt).fuse() => {}
        }
    }
    Ok(())
}

#[allow(clippy::cognitive_complexity)]
pub async fn handler(
    core: &mut Core<'_>,
    output_stream: Sender<CheckedPacket>,
    awaits_halt: &mut bool,
    packet: CheckedPacket,
) -> ServerResult<bool> {
    let mut break_due = false;
    if packet.is_valid() {
        let packet_string = String::from_utf8_lossy(&packet.data).to_string();
        #[allow(clippy::if_same_then_else)]
        let response: Option<String> = if packet.data.starts_with(b"qSupported") {
            handlers::q_supported()
        } else if packet.data.starts_with(b"vMustReplyEmpty") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qTStatus") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qTfV") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qAttached") {
            handlers::q_attached()
        } else if packet.data.starts_with(b"?") {
            handlers::halt_reason()
        } else if packet.data.starts_with(b"g") {
            handlers::read_general_registers()
        } else if packet.data.starts_with(b"p") {
            handlers::read_register(packet_string, core)
        } else if packet.data.starts_with(b"qTsP") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qfThreadInfo") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"m") {
            handlers::read_memory(packet_string, core)
        } else if packet.data.starts_with(b"qL") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qC") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qOffsets") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"vCont?") {
            handlers::vcont_supported()
        } else if packet.data.starts_with(b"vContb;c") || packet.data.starts_with(b"c") {
            handlers::run(core, awaits_halt)
        } else if packet.data.starts_with(b"vContb;t") {
            handlers::stop(core, awaits_halt)
        } else if packet.data.starts_with(b"vContb;s") || packet.data.starts_with(b"s") {
            handlers::step(core, awaits_halt)
        } else if packet.data.starts_with(b"Z0") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"Z1") {
            handlers::insert_hardware_break(packet_string, core)
        } else if packet.data.starts_with(b"z1") {
            handlers::remove_hardware_break(packet_string, core)
        } else if packet.data.starts_with(b"X") {
            handlers::write_memory(packet_string, &packet.data, core)
        } else if packet.data.starts_with(b"qXfer:memory-mapb:read") {
            handlers::get_memory_map()
        } else if packet.data.starts_with(&[0x03]) {
            handlers::user_halt(core, awaits_halt)
        } else if packet.data.starts_with(b"D") {
            handlers::detach(&mut break_due)
        } else if packet.data.starts_with(b"qRcmdb,7265736574") {
            handlers::reset_halt(core)
        } else if packet.data.starts_with(b"qTfV") {
            handlers::reply_empty()
        } else if packet.data.starts_with(b"qTfV") {
            handlers::reply_empty()
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
    Ok(break_due)
}

pub async fn await_halt(
    core: &mut Core<'_>,
    output_stream: Sender<CheckedPacket>,
    await_halt: bool,
) {
    if await_halt && core.core_halted().unwrap() {
        let response =
            CheckedPacket::from_data(PacketKind::Packet, "T05hwbreak:;".to_string().into_bytes());

        let mut bytes = Vec::new();
        response.encode(&mut bytes).unwrap();

        let _ = output_stream.unbounded_send(response);
    }
}
