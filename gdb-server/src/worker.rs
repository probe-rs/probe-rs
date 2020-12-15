use async_std::prelude::*;
use async_std::task;
use futures::channel::mpsc;
use futures::future::FutureExt;
use futures::select;
use gdb_protocol::packet::{CheckedPacket, Kind as PacketKind};
use probe_rs::Session;
use std::convert::TryFrom;
use std::{sync::Mutex, time::Duration};

use crate::parser::parse_packet;

use crate::handlers;

type ServerResult<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

pub async fn worker(
    mut input_stream: Receiver<CheckedPacket>,
    output_stream: Sender<CheckedPacket>,
    session: &Mutex<Session>,
) -> ServerResult<()> {
    // When we first attach to the core, GDB expects us to halt the core, so we do this here when a new client connects.
    // If the core is already halted, nothing happens if we issue a halt command again, so we always do this no matter of core state.
    session
        .lock()
        .unwrap()
        .core(0)?
        .halt(Duration::from_millis(100))?;

    let mut awaits_halt = false;

    loop {
        select! {
            potential_packet = input_stream.next().fuse() => {
                if let Some(packet) = potential_packet {
                    log::warn!("WORKING {}", String::from_utf8_lossy(&packet.data));
                    if handler(&session, &output_stream, &mut awaits_halt, packet).await? {
                        break;
                    }
                } else {
                    break
                }
            },
            _ = await_halt(session, &output_stream, &mut awaits_halt).fuse() => {}
        }
    }
    Ok(())
}

pub async fn handler(
    session: &Mutex<Session>,
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
            let mut session = session.lock().expect("Poisoned Mutex");
            match parsed_packet {
                HaltReason => handlers::halt_reason(),
                Continue => handlers::run(session.core(0)?, awaits_halt),
                V(VPacket::QueryContSupport) => handlers::vcont_supported(),
                Query(QueryPacket::Supported { .. }) => handlers::q_supported(),
                Query(QueryPacket::Attached { .. }) => handlers::q_attached(),
                Query(QueryPacket::Command(cmd)) => {
                    if cmd == b"reset" {
                        handlers::reset_halt(session.core(0)?)
                    } else {
                        log::debug!("Unknown monitor command: '{:?}'", cmd);
                        Some(hex::encode(
                            "Unknown monitor command\nOnly 'reset' is currently supported\n"
                                .as_bytes(),
                        ))
                    }
                }
                Query(QueryPacket::HostInfo) => handlers::host_info(),
                ReadGeneralRegister => handlers::read_general_registers(session.core(0)?),
                ReadRegisterHex(register) => handlers::read_register(register, session.core(0)?),
                ReadMemory { address, length } => {
                    // LLDB will send 64 bit addresses, which are not supported by probe-rs
                    // yet.

                    if let Ok(address) = u32::try_from(address) {
                        handlers::read_memory(address, length, session.core(0)?)
                    } else {
                        //
                        handlers::reply_empty()
                    }
                }
                Detach => handlers::detach(&mut break_due),
                V(VPacket::Continue(action)) => match action {
                    Action::Continue => handlers::run(session.core(0)?, awaits_halt),
                    Action::Stop => handlers::stop(session.core(0)?, awaits_halt),
                    Action::Step => handlers::step(session.core(0)?, awaits_halt),
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
                        handlers::insert_hardware_break(address, kind, session.core(0)?)
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
                        handlers::remove_hardware_break(address, kind, session.core(0)?)
                    }
                    other => {
                        log::warn!("Breakpoint type {:?} is not supported.", other);
                        handlers::reply_empty()
                    }
                },
                WriteMemoryBinary { address, data } => {
                    handlers::write_memory(address, &data, session.core(0)?)
                }
                Query(QueryPacket::Transfer { object, operation }) => {
                    use crate::parser::query::TransferOperation;

                    match object.as_slice() {
                        b"memory-map" => {
                            match operation {
                                TransferOperation::Read { .. } => {
                                    handlers::get_memory_map(&session)
                                }
                                TransferOperation::Write { .. } => {
                                    // not supported
                                    handlers::reply_empty()
                                }
                            }
                        }
                        b"features" => {
                            match operation {
                                TransferOperation::Read { annex, .. } => {
                                    handlers::read_target_description(&session, &annex)
                                }
                                TransferOperation::Write { .. } => {
                                    // not supported
                                    handlers::reply_empty()
                                }
                            }
                        }
                        object => {
                            log::warn!("Object '{:?}' not supported for qXfer command", object);
                            handlers::reply_empty()
                        }
                    }
                }
                Interrupt => handlers::user_halt(session.core(0)?, awaits_halt),
                other => {
                    log::warn!("Unknown command: '{:?}'", other);

                    // respond with an empty response to indicate that we don't suport the command
                    handlers::reply_empty()
                }
            }
        }
        Err(e) => {
            log::warn!(
                "Failed to parse packet '{:?}': {}",
                String::from_utf8_lossy(&packet.data),
                e
            );
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
    session: &Mutex<Session>,
    output_stream: &Sender<CheckedPacket>,
    await_halt: &mut bool,
) -> ServerResult<()> {
    task::sleep(Duration::from_millis(10)).await;
    if *await_halt {
        let mut session = session.lock().expect("Poisoned Mutex");
        if session.core(0)?.core_halted().unwrap() {
            let response = CheckedPacket::from_data(PacketKind::Packet, b"T05hwbreak:;".to_vec());

            let mut bytes = Vec::new();
            response.encode(&mut bytes).unwrap();
            *await_halt = false;

            let _ = output_stream.unbounded_send(response);
        }
    }

    Ok(())
}
