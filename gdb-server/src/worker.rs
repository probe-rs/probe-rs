use async_std::prelude::*;
use async_std::task;
use futures::channel::mpsc;
use futures::future::FutureExt;
use futures::select;
use gdb_protocol::packet::{CheckedPacket, Kind as PacketKind};
use probe_rs::Core;
use probe_rs::Session;
use std::convert::TryFrom;
use std::time::Duration;

use crate::parser::parse_packet;

use crate::handlers;

type ServerResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

pub async fn worker(
    mut input_stream: Receiver<CheckedPacket>,
    output_stream: Sender<CheckedPacket>,
    session: &mut Session,
) -> ServerResult<()> {
    let mut core = session.core(0).unwrap();
    let mut awaits_halt = false;

    loop {
        select! {
            potential_packet = input_stream.next().fuse() => {
                if let Some(packet) = potential_packet {
                    log::warn!("WORKING {}", String::from_utf8_lossy(&packet.data));
                    if handler(&mut core, &output_stream, &mut awaits_halt, packet).await? {
                        break;
                    }
                } else {
                    break
                }
            },
            _ = await_halt(&mut core, &output_stream, &mut awaits_halt).fuse() => {}
        }
    }
    Ok(())
}

#[allow(clippy::cognitive_complexity)]
pub async fn handler(
    core: &mut Core<'_>,
    output_stream: &Sender<CheckedPacket>,
    awaits_halt: &mut bool,
    packet: CheckedPacket,
) -> ServerResult<bool> {
    let parsed_packet = parse_packet(&packet.data);
    let mut break_due = false;

    use crate::parser::v_packet::Action;
    use crate::parser::BreakpointType;
    use crate::parser::Packet::*;
    use crate::parser::QueryPacket;
    use crate::parser::VPacket;

    let response: Option<String> = match parsed_packet {
        Ok(parsed_packet) => {
            log::debug!("Parsed packet: {:?}", parsed_packet);
            match parsed_packet {
                HaltReason => handlers::halt_reason(),
                Continue => handlers::run(core, awaits_halt),
                V(VPacket::QueryContSupport) => handlers::vcont_supported(),
                Query(QueryPacket::Supported { .. }) => handlers::q_supported(),
                Query(QueryPacket::Attached { .. }) => handlers::q_attached(),
                Query(QueryPacket::Command(cmd)) => {
                    if cmd == b"reset" {
                        handlers::reset_halt(core)
                    } else {
                        log::debug!("Unknown monitor command: '{:?}'", cmd);
                        handlers::reply_empty()
                    }
                }
                Query(QueryPacket::HostInfo) => handlers::host_info(),
                ReadGeneralRegister => handlers::read_general_registers(),
                ReadRegisterHex(register) => handlers::read_register(register, core),
                ReadMemory { address, length } => {
                    // LLDB will send 64 bit addresses, which are not supported by probe-rs
                    // yet.

                    if let Ok(address) = u32::try_from(address) {
                        handlers::read_memory(address, length, core)
                    } else {
                        //
                        handlers::reply_empty()
                    }
                }
                Detach => handlers::detach(&mut break_due),
                V(VPacket::Continue(action)) => match action {
                    Action::Continue => handlers::run(core, awaits_halt),
                    Action::Stop => handlers::stop(core, awaits_halt),
                    Action::Step => handlers::step(core, awaits_halt),
                    other => {
                        log::warn!("vCont with action {:?} not supported", other);
                        handlers::reply_empty()
                    }
                },
                InsertBreakpoint {
                    breakpoint_type,
                    address,
                    kind,
                } => match breakpoint_type {
                    BreakpointType::Hardware => {
                        handlers::insert_hardware_break(address, kind, core)
                    }
                    other => {
                        log::warn!("Breakpoint type {:?} is not supported.", other);
                        handlers::reply_empty()
                    }
                },
                RemoveBreakpoint {
                    breakpoint_type,
                    address,
                    kind,
                } => match breakpoint_type {
                    BreakpointType::Hardware => {
                        handlers::remove_hardware_break(address, kind, core)
                    }
                    other => {
                        log::warn!("Breakpoint type {:?} is not supported.", other);
                        handlers::reply_empty()
                    }
                },
                WriteMemoryBinary { address, data } => handlers::write_memory(address, &data, core),
                Query(QueryPacket::Transfer { object, operation }) => {
                    use crate::parser::query::TransferOperation;

                    if object == b"memory-map" {
                        match operation {
                            TransferOperation::Read { .. } => handlers::get_memory_map(),
                            TransferOperation::Write { .. } => {
                                // not supported
                                handlers::reply_empty()
                            }
                        }
                    } else {
                        log::warn!("Object '{:?}' not supported for qXfer command", object);
                        handlers::reply_empty()
                    }
                }
                Interrupt => handlers::user_halt(core, awaits_halt),
                other => {
                    log::warn!("Unknown command: '{:?}'", other);

                    // respond with an empty response to indicate that we don't suport the command
                    handlers::reply_empty()
                }
            }
        }
        Err(e) => {
            log::warn!("Failed to parse packet '{:?}': {}", &packet.data, e);
            handlers::reply_empty()
        }
    };

    if let Some(response) = response {
        let response = CheckedPacket::from_data(PacketKind::Packet, response.into_bytes());

        let mut bytes = Vec::new();
        response.encode(&mut bytes).unwrap();
        log::debug!(
            "Response: '{:x?}'",
            std::str::from_utf8(&response.data).unwrap()
        );
        log::debug!("-----------------------------------------------");
        output_stream.unbounded_send(response)?;
    };

    Ok(break_due)
}

pub async fn await_halt(
    core: &mut Core<'_>,
    output_stream: &Sender<CheckedPacket>,
    await_halt: &mut bool,
) {
    task::sleep(Duration::from_millis(10)).await;
    if *await_halt && core.core_halted().unwrap() {
        let response = CheckedPacket::from_data(PacketKind::Packet, b"T05hwbreak:;".to_vec());

        let mut bytes = Vec::new();
        response.encode(&mut bytes).unwrap();
        *await_halt = false;

        let _ = output_stream.unbounded_send(response);
    }
}
