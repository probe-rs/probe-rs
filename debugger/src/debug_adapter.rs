use crate::debugger::ConsoleLog;
use crate::debugger::CoreData;
use crate::DebuggerError;
use crate::{dap_types, rtt::DataFormat};
use anyhow::{anyhow, Result};
use dap_types::*;
use parse_int::parse;
use probe_rs::{
    debug::{ColumnType, VariableKind},
    CoreStatus, HaltReason, MemoryInterface,
};
use serde::{de::DeserializeOwned, Serialize};

use std::{collections::HashMap, string::ToString};
use std::{
    convert::TryInto,
    path::{Path, PathBuf},
    str, thread,
    time::Duration,
};

use crate::protocol::ProtocolAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAdapterType {
    CommandLine,
    DapClient,
}

/// Progress ID used for progress reporting when the debug adapter protocol is used.
type ProgressId = i64;

pub struct DebugAdapter<P: ProtocolAdapter> {
    /// Track the last_known_status of the probe.
    /// The debug client needs to be notified when the probe changes state,
    /// and the only way is to poll the probe status periodically.
    /// For instance, when the client sets the probe running,
    /// and the probe halts because of a breakpoint, we need to notify the client.
    pub(crate) last_known_status: CoreStatus,
    pub(crate) halt_after_reset: bool,
    /// `scope_map` stores a list of all MS DAP Scopes with a each stack frame's unique id as key.
    /// It is cleared by `threads()`, populated by stack_trace(), for later re-use by `scopes()`.
    scope_map: HashMap<i64, Vec<Scope>>,
    /// `variable_map` stores a list of all MS DAP Variables with a unique per-level reference.
    /// It is cleared by `threads()`, populated by stack_trace(), for later nested re-use by `variables()`.
    variable_map_key_seq: i64, // Used to create unique values for `self.variable_map` keys.
    variable_map: HashMap<i64, Vec<Variable>>,

    progress_id: ProgressId,

    /// Flag to indicate if the connected client supports progress reporting.
    pub(crate) supports_progress_reporting: bool,
    adapter: P,
}

impl<P: ProtocolAdapter> DebugAdapter<P> {
    pub fn new(adapter: P) -> DebugAdapter<P> {
        DebugAdapter {
            last_known_status: CoreStatus::Unknown,
            halt_after_reset: false,
            scope_map: HashMap::new(),
            variable_map: HashMap::new(),
            variable_map_key_seq: -1,
            progress_id: 0,
            supports_progress_reporting: false,
            adapter,
        }
    }

    pub(crate) fn adapter_type(&self) -> DebugAdapterType {
        P::ADAPTER_TYPE
    }

    pub(crate) fn status(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let status = match core_data.target_core.status() {
            Ok(status) => {
                self.last_known_status = status;
                status
            }
            Err(error) => {
                return self
                    .send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Could not read core status. {:?}",
                            error
                        ))),
                    )
                    .is_ok()
            }
        };
        if status.is_halted() {
            let pc = core_data
                .target_core
                .read_core_reg(core_data.target_core.registers().program_counter());
            match pc {
                Ok(pc) => self
                    .send_response(
                        request,
                        Ok(Some(format!(
                            "Status: {:?} at address {:#010x}",
                            status.short_long_status().1,
                            pc
                        ))),
                    )
                    .is_ok(),
                Err(error) => self
                    .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
                    .is_ok(),
            }
        } else {
            self.send_response(request, Ok(Some(status.short_long_status().1.to_string())))
                .is_ok()
        }
    }

    pub(crate) fn pause(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        // let args: PauseArguments = get_arguments(&request)?;

        match core_data.target_core.halt(Duration::from_millis(500)) {
            Ok(cpu_info) => {
                let event_body = Some(StoppedEventBody {
                    reason: "pause".to_owned(),
                    description: Some(self.last_known_status.short_long_status().1.to_owned()),
                    thread_id: Some(core_data.target_core.id() as i64),
                    preserve_focus_hint: Some(false),
                    text: None,
                    all_threads_stopped: Some(true),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body);
                self.send_response(
                    request,
                    Ok(Some(format!(
                        "Core stopped at address 0x{:08x}",
                        cpu_info.pc
                    ))),
                );
                self.last_known_status = CoreStatus::Halted(HaltReason::Request);

                true
            }
            Err(error) => self
                .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
                .is_ok(),
        }

        // TODO: This is from original probe_rs_cli 'halt' function ... disasm code at memory location
        /*
        let mut code = [0u8; 16 * 2];

        core_data.target_core.read(cpu_info.pc, &mut code)?;

        let instructions = core_data
            .capstone
            .disasm_all(&code, u64::from(cpu_info.pc))
            .unwrap();

        for i in instructions.iter() {
            println!("{}", i);
        }


        for (offset, instruction) in code.iter().enumerate() {
            println!(
                "{:#010x}: {:010x}",
                cpu_info.pc + offset as u32,
                instruction
            );
        }
            */
    }

    pub(crate) fn read_memory(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let arguments: ReadMemoryArguments = match self.adapter_type() {
            DebugAdapterType::CommandLine => match request.arguments.as_ref().unwrap().try_into() {
                Ok(arguments) => arguments,
                Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
            },
            DebugAdapterType::DapClient => match get_arguments(request) {
                Ok(arguments) => arguments,
                Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
            },
        };
        let address: u32 = parse(arguments.memory_reference.as_ref()).unwrap();
        let num_words = arguments.count as usize;
        let mut buff = vec![0u32; num_words];
        if num_words > 1 {
            core_data.target_core.read_32(address, &mut buff).unwrap();
        } else {
            buff[0] = core_data.target_core.read_word_32(address).unwrap();
        }
        if !buff.is_empty() {
            let mut response = "".to_string();
            for (offset, word) in buff.iter().enumerate() {
                response.push_str(
                    format!("0x{:08x} = 0x{:08x}\n", address + (offset * 4) as u32, word).as_str(),
                );
            }
            self.send_response::<String>(request, Ok(Some(response)))
                .is_ok()
        } else {
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Could not read any data at address 0x{:08x}",
                    address
                ))),
            )
            .is_ok()
        }
    }
    pub(crate) fn write(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };
        let data = match get_int_argument(request.arguments.as_ref(), "data", 1) {
            Ok(data) => data,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };

        match core_data
            .target_core
            .write_word_32(address, data)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => true,
            Err(error) => self.send_response::<()>(request, Err(error)).is_ok(),
        }
    }
    pub(crate) fn set_breakpoint(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };

        match core_data
            .target_core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => {
                return self
                    .send_response(
                        request,
                        Ok(Some(format!(
                            "Set new breakpoint at address {:#08x}",
                            address
                        ))),
                    )
                    .is_ok();
            }
            Err(error) => self.send_response::<()>(request, Err(error)).is_ok(),
        }
    }
    pub(crate) fn clear_breakpoint(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };

        match core_data
            .target_core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => true,
            Err(error) => self.send_response::<()>(request, Err(error)).is_ok(),
        }
    }

    pub(crate) fn show_cpu_register_values(
        &mut self,
        _core_data: &mut CoreData,
        _request: &Request,
    ) -> bool {
        todo!();
        // let register_file = core_data.target_core.registers();

        // for register in register_file.registers() {
        //     let value = match core_data.target_core.read_core_reg(register) {
        //         Ok(value) => {
        //             println!("{}: {:#010x}", register.name(), value);
        //         }
        //         Err(error) => return Err(DebuggerError::Other(anyhow!("{}", error))),
        //     };
        // }
        // true
    }
    pub(crate) fn dump_cpu_state(
        &mut self,
        _core_data: &mut CoreData,
        _requestt: &Request,
    ) -> bool {
        todo!();
        // dump all relevant data, stack and regs for now..
        //
        // stack beginning -> assume beginning to be hardcoded

        // let stack_top: u32 = 0x2000_0000 + 0x4000;

        // let regs = core_data.target_core.registers();

        // let stack_bot: u32 = core_data.target_core.read_core_reg(regs.stack_pointer())?;
        // let pc: u32 = core_data
        //     .target_core
        //     .read_core_reg(regs.program_counter())?;

        // let mut stack = vec![0u8; (stack_top - stack_bot) as usize];

        // core_data.target_core.read(stack_bot, &mut stack[..])?;

        // let mut dump = Dump::new(stack_bot, stack);

        // for i in 0..12 {
        //     dump.regs[i as usize] = core_data
        //         .target_core
        //         .read_core_reg(Into::<CoreRegisterAddress>::into(i))?;
        // }

        // dump.regs[13] = stack_bot;
        // dump.regs[14] = core_data.target_core.read_core_reg(regs.return_address())?;
        // dump.regs[15] = pc;

        // let serialized = ron::ser::to_string(&dump).expect("Failed to serialize dump");

        // let mut dump_file = File::create("dump.txt").expect("Failed to create file");

        // dump_file
        //     .write_all(serialized.as_bytes())
        //     .expect("Failed to write dump file");
        // true
    }
    pub(crate) fn restart(&mut self, core_data: &mut CoreData, request: Option<&Request>) -> bool {
        match core_data.target_core.halt(Duration::from_millis(500)) {
            Ok(_) => {}
            Err(error) => {
                if let Some(request) = request {
                    return self
                        .send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("{}", error))),
                        )
                        .is_ok();
                } else {
                    return self
                        .send_error_response(&DebuggerError::Other(anyhow!("{}", error)))
                        .is_ok();
                }
            }
        }

        if request.is_some() || self.adapter_type() == DebugAdapterType::CommandLine {
            match core_data.target_core.reset() {
                Ok(_) => {
                    self.last_known_status = CoreStatus::Running;
                    let event_body = Some(ContinuedEventBody {
                        all_threads_continued: Some(true),
                        thread_id: core_data.target_core.id() as i64,
                    });

                    self.send_event("continued", event_body).is_ok()
                }
                Err(error) => {
                    return self
                        .send_response::<()>(
                            request.unwrap(), // Checked above
                            Err(DebuggerError::Other(anyhow!("{}", error))),
                        )
                        .is_ok();
                }
            }
        } else if self.halt_after_reset || self.adapter_type() == DebugAdapterType::DapClient
        // The DAP Client will always do a `reset_and_halt`, and then will consider `halt_after_reset` value after the `configuration_done` request.
        // Otherwise the probe will run past the `main()` before the DAP Client has had a chance to set breakpoints in `main()`.
        {
            match core_data
                .target_core
                .reset_and_halt(Duration::from_millis(500))
            {
                Ok(_) => {
                    match self.adapter_type() {
                        DebugAdapterType::CommandLine => {}
                        DebugAdapterType::DapClient => {
                            if let Some(request) = request {
                                return self.send_response::<()>(request, Ok(None)).is_ok();
                            }
                        }
                    }
                    // Only notify the DAP client if we are NOT in initialization stage (`CoreStatus::Unknown`).
                    if self.last_known_status != CoreStatus::Unknown {
                        let event_body = Some(StoppedEventBody {
                            reason: "reset".to_owned(),
                            description: Some(
                                CoreStatus::Halted(HaltReason::External)
                                    .short_long_status()
                                    .1
                                    .to_string(),
                            ),
                            thread_id: Some(core_data.target_core.id() as i64),
                            preserve_focus_hint: None,
                            text: None,
                            all_threads_stopped: Some(true),
                            hit_breakpoint_ids: None,
                        });
                        self.send_event("stopped", event_body);
                        self.last_known_status = CoreStatus::Halted(HaltReason::External);
                    }
                    true
                }
                Err(error) => {
                    if let Some(request) = request {
                        return self
                            .send_response::<()>(
                                request,
                                Err(DebuggerError::Other(anyhow!("{}", error))),
                            )
                            .is_ok();
                    } else {
                        return self
                            .send_error_response(&DebuggerError::Other(anyhow!("{}", error)))
                            .is_ok();
                    }
                }
            }
        } else {
            true
        }
    }

    pub(crate) fn configuration_done(
        &mut self,
        core_data: &mut CoreData,
        request: &Request,
    ) -> bool {
        // Make sure the DAP Client and the DAP Server are in sync with the status of the core.
        match core_data.target_core.status() {
            Ok(core_status) => {
                self.last_known_status = core_status;
                if core_status.is_halted() {
                    if self.halt_after_reset
                        || core_status == CoreStatus::Halted(HaltReason::Breakpoint)
                    {
                        self.send_response::<()>(request, Ok(None));
                        let event_body = Some(StoppedEventBody {
                            reason: core_status.short_long_status().0.to_owned(),
                            description: Some(core_status.short_long_status().1.to_string()),
                            thread_id: Some(core_data.target_core.id() as i64),
                            preserve_focus_hint: None,
                            text: None,
                            all_threads_stopped: Some(true),
                            hit_breakpoint_ids: None,
                        });
                        self.send_event("stopped", event_body).is_ok()
                    } else {
                        self.r#continue(core_data, request)
                    }
                } else {
                    self.send_response::<()>(request, Ok(None)).is_ok()
                }
            }
            Err(error) => {
                self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read core status to synchronize the client and the probe. {:?}",
                        error
                    ))),
                );
                false
            }
        }
    }
    pub(crate) fn threads(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        // TODO: Implement actual thread resolution. For now, we just use the core id as the thread id.

        let single_thread = Thread {
            id: core_data.target_core.id() as i64,
            name: core_data.target_name.clone(),
        };

        let threads = vec![single_thread];
        self.scope_map.clear();
        self.variable_map.clear();
        self.variable_map_key_seq = -1;
        self.send_response(request, Ok(Some(ThreadsResponseBody { threads })))
            .is_ok()
    }
    pub(crate) fn set_breakpoints(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let args: SetBreakpointsArguments = match get_arguments(request) {
            Ok(arguments) => arguments,
            Err(error) => {
                return self
                    .send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Could not read arguments : {}",
                            error
                        ))),
                    )
                    .is_ok()
            }
        };

        let mut created_breakpoints: Vec<Breakpoint> = Vec::new(); // For returning in the Response

        let source_path = args.source.path.as_ref().map(Path::new);

        // Always clear existing breakpoints before setting new ones. The DAP Specification doesn't make allowances for deleting and setting individual breakpoints.
        match core_data.target_core.clear_all_hw_breakpoints() {
            Ok(_) => {}
            Err(error) => {
                return self
                    .send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Failed to clear existing breakpoints before setting new ones : {}",
                            error
                        ))),
                    )
                    .is_ok()
            }
        }

        if let Some(requested_breakpoints) = args.breakpoints.as_ref() {
            for bp in requested_breakpoints {
                // Try to find source code location

                let source_location: Option<u64> = core_data.debug_info.as_ref().and_then(|di| {
                    di.get_breakpoint_location(
                        source_path.unwrap(),
                        bp.line as u64,
                        bp.column.map(|c| c as u64),
                    )
                    .unwrap_or(None)
                });

                if let Some(location) = source_location {
                    let (verified, reason_msg) =
                        match core_data.target_core.set_hw_breakpoint(location as u32) {
                            Ok(_) => (
                                true,
                                Some(format!("Breakpoint at memory address: 0x{:08x}", location)),
                            ),
                            Err(err) => {
                                let message = format!(
                                "WARNING: Could not set breakpoint at memory address: 0x{:08x}: {}",
                                location, err
                            )
                                .to_string();
                                // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                                self.log_to_console(format!("WARNING: {}", message));
                                self.show_message(MessageSeverity::Warning, message.clone());
                                (false, Some(message))
                            }
                        };

                    created_breakpoints.push(Breakpoint {
                        column: bp.column,
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(bp.line),
                        message: reason_msg,
                        source: None,
                        instruction_reference: Some(location.to_string()),
                        offset: None,
                        verified,
                    });
                } else {
                    let message = "No source location for breakpoint. Try reducing `opt-level` in `Cargo.toml` ".to_string();
                    // In addition to sending the error to the 'Hover' message, also write it to the Debug Console Log.
                    self.log_to_console(format!("WARNING: {}", message));
                    self.show_message(MessageSeverity::Warning, message.clone());
                    created_breakpoints.push(Breakpoint {
                        column: bp.column,
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(bp.line),
                        message: Some(message),
                        source: None,
                        instruction_reference: None,
                        offset: None,
                        verified: false,
                    });
                }
            }
        }

        let breakpoint_body = SetBreakpointsResponseBody {
            breakpoints: created_breakpoints,
        };
        self.send_response(request, Ok(Some(breakpoint_body)))
            .is_ok()
    }

    pub(crate) fn stack_trace(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        let _status = match core_data.target_core.status() {
            Ok(status) => {
                if !status.is_halted() {
                    return self
                        .send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!(
                                "Core must be halted before requesting a stack trace"
                            ))),
                        )
                        .is_ok();
                }
            }
            Err(error) => {
                return self
                    .send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
                    .is_ok()
            }
        };

        let regs = core_data.target_core.registers();

        let pc = match core_data.target_core.read_core_reg(regs.program_counter()) {
            Ok(pc) => pc,
            Err(error) => {
                return self
                    .send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
                    .is_ok()
            }
        };

        let _arguments: StackTraceArguments = match self.adapter_type() {
            DebugAdapterType::CommandLine => StackTraceArguments {
                format: None,
                levels: None,
                start_frame: None,
                thread_id: core_data.target_core.id() as i64,
            },
            DebugAdapterType::DapClient => match get_arguments(request) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return self
                        .send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!(
                                "Could not read arguments : {}",
                                error
                            ))),
                        )
                        .is_ok()
                }
            },
        };

        if let Some(debug_info) = core_data.debug_info.as_ref() {
            // Evaluate the static scoped variables.
            let static_variables =
                match debug_info.get_stack_statics(&mut core_data.target_core, u64::from(pc)) {
                    Ok(static_variables) => static_variables,
                    Err(err) => {
                        let mut error_variable = probe_rs::debug::Variable::new();
                        error_variable.name = "ERROR".to_string();
                        error_variable
                            .set_value(format!("Failed to retrieve static variables: {:?}", err));
                        vec![error_variable]
                    }
                };

            // Store the static variables for later calls to `variables()` to retrieve.
            let (static_scope_reference, named_static_variables_cnt, indexed_static_variables_cnt) =
                self.create_variable_map(&static_variables);

            let current_stackframes =
                debug_info.try_unwind(&mut core_data.target_core, u64::from(pc));

            match self.adapter_type() {
                DebugAdapterType::CommandLine => {
                    let mut body = "".to_string();
                    // TODO: Update the code to include static variables.
                    for frame in current_stackframes {
                        body.push_str(format!("{}\n", frame).as_str());
                    }
                    self.send_response(request, Ok(Some(body))).is_ok()
                }
                DebugAdapterType::DapClient => {
                    let mut frame_list: Vec<StackFrame> = current_stackframes
                        .map(|frame| {
                            let column = frame
                                .source_location
                                .as_ref()
                                .and_then(|sl| sl.column)
                                .map(|col| match col {
                                    ColumnType::LeftEdge => 0,
                                    ColumnType::Column(c) => c,
                                })
                                .unwrap_or(0);

                            let source = if let Some(source_location) = &frame.source_location {
                                let path: Option<PathBuf> =
                                    source_location.directory.as_ref().map(|path| {
                                        let mut path = if path.is_relative() {
                                            std::env::current_dir().unwrap().join(path)
                                        } else {
                                            path.to_owned()
                                        };

                                        if let Some(file) = &source_location.file {
                                            path.push(file);
                                        }

                                        path
                                    });
                                Some(Source {
                                    name: source_location.file.clone(),
                                    path: path.map(|p| p.to_string_lossy().to_string()),
                                    source_reference: None,
                                    presentation_hint: None,
                                    origin: None,
                                    sources: None,
                                    adapter_data: None,
                                    checksums: None,
                                })
                            } else {
                                log::debug!("No source location present for frame!");
                                None
                            };

                            let line = frame
                                .source_location
                                .as_ref()
                                .and_then(|sl| sl.line)
                                .unwrap_or(0) as i64;

                            // MS DAP requests happen in the order Threads -> StackFrames -> Scopes -> Variables (recursive).
                            // We build & extract all the info during this `stack_trace()` method, and re-use it when MS DAP requests come in.
                            let mut scopes = vec![];

                            // Build the locals scope.
                            // Extract all the variables from the `StackFrame` for later MS DAP calls to retrieve.
                            let (variables_reference, named_variables_cnt, indexed_variables_cnt) =
                                self.create_variable_map(&frame.variables);

                            scopes.push(Scope {
                                line: Some(line),
                                column: frame.source_location.as_ref().and_then(|l| {
                                    l.column.map(|c| match c {
                                        ColumnType::LeftEdge => 0,
                                        ColumnType::Column(c) => c as i64,
                                    })
                                }),
                                end_column: None,
                                end_line: None,
                                expensive: false,
                                indexed_variables: Some(indexed_variables_cnt),
                                name: "Locals".to_string(),
                                presentation_hint: Some("locals".to_string()),
                                named_variables: Some(named_variables_cnt),
                                source: source.clone(),
                                variables_reference,
                            });

                            // The static variables are mapped and stored before iterating the frames. Store a reference to them here.
                            scopes.push(Scope {
                                line: None,
                                column: None,
                                end_column: None,
                                end_line: None,
                                expensive: true, // VSCode won't open this tree by default.
                                indexed_variables: Some(indexed_static_variables_cnt),
                                name: "Static".to_string(),
                                presentation_hint: Some("statics".to_string()),
                                named_variables: Some(named_static_variables_cnt),
                                source: None,
                                variables_reference: if indexed_static_variables_cnt
                                    + named_variables_cnt
                                    == 0
                                {
                                    0
                                } else {
                                    static_scope_reference
                                },
                            });

                            // Build the registers scope and add its variables.
                            // TODO: Consider expanding beyond core registers to add other architectue registers.
                            let register_scope_reference = self.new_variable_map_key();

                            // TODO: This is ARM specific, but should be generalized
                            let variables: Vec<Variable> = frame
                                .registers
                                .registers()
                                .map(|(register_number, value)| Variable {
                                    name: match register_number {
                                        7 => "R7: THUMB Frame Pointer".to_owned(),
                                        11 => "R11: ARM Frame Pointer".to_owned(),
                                        13 => "SP".to_owned(),
                                        14 => "LR".to_owned(),
                                        15 => "PC".to_owned(),
                                        other => format!("R{}", other),
                                    },
                                    value: format!("0x{:08x}", value),
                                    type_: Some("Core Register".to_owned()),
                                    presentation_hint: None,
                                    evaluate_name: None,
                                    variables_reference: 0,
                                    named_variables: None,
                                    indexed_variables: None,
                                    memory_reference: None,
                                })
                                .collect();

                            let register_count = variables.len();

                            self.variable_map
                                .insert(register_scope_reference, variables);
                            scopes.push(Scope {
                                line: None,
                                column: None,
                                end_column: None,
                                end_line: None,
                                expensive: true, // VSCode won't open this tree by default.
                                indexed_variables: Some(0),
                                name: "Registers".to_string(),
                                presentation_hint: Some("registers".to_string()),
                                named_variables: Some(register_count as i64),
                                source: None,
                                variables_reference: if register_count > 0 {
                                    register_scope_reference
                                } else {
                                    0
                                },
                            });

                            // Finally, store the scopes for this frame.
                            self.scope_map.insert(frame.id as i64, scopes);

                            // TODO: Can we add more meaningful info to `module_id`, etc.
                            StackFrame {
                                id: frame.id as i64,
                                name: frame.function_name.clone(),
                                source,
                                line,
                                column: column as i64,
                                end_column: None,
                                end_line: None,
                                module_id: None,
                                presentation_hint: Some("normal".to_owned()),
                                can_restart: Some(false),
                                instruction_pointer_reference: Some(format!("0x{:08x}", frame.pc)),
                            }
                        })
                        .collect();

                    // If we get an empty stack frame list,
                    // add a frame so that something is visible in the
                    // debugger.
                    if frame_list.is_empty() {
                        frame_list.push(StackFrame {
                            can_restart: None,
                            column: 0,
                            end_column: None,
                            end_line: None,
                            id: pc as i64,
                            instruction_pointer_reference: None,
                            line: 0,
                            module_id: None,
                            name: format!("<unknown function @ {:#010x}>", pc),
                            presentation_hint: None,
                            source: None,
                        })
                    }

                    let frame_len = frame_list.len();

                    let body = StackTraceResponseBody {
                        stack_frames: frame_list,
                        total_frames: Some(frame_len as i64),
                    };
                    self.send_response(request, Ok(Some(body))).is_ok()
                }
            }
        } else {
            // No debug information, so we cannot send stack trace information
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!("No debug information found!"))),
            )
            .is_ok()
        }
    }
    /// Retrieve available scopes  
    /// - local scope   : Variables defined between start of current frame, and the current pc (program counter)
    /// - static scope  : Variables with `static` modifier
    /// - registers     : Currently supports core registers 0-15
    pub(crate) fn scopes(&mut self, _core_data: &mut CoreData, request: &Request) -> bool {
        let arguments: ScopesArguments = match get_arguments(request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };

        match self.scope_map.clone().get(&(arguments.frame_id)) {
            Some(dap_scopes) => self
                .send_response(
                    request,
                    Ok(Some(ScopesResponseBody {
                        scopes: dap_scopes.clone(),
                    })),
                )
                .is_ok(),
            None => self
                .send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "No variable information available"
                    ))),
                )
                .is_ok(),
        }
    }
    pub(crate) fn source(&mut self, _core_data: &mut CoreData, request: &Request) -> bool {
        let arguments: SourceArguments = match get_arguments(request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };

        let result = if let Some(path) = arguments.source.and_then(|s| s.path) {
            let mut source_path = PathBuf::from(path);

            if source_path.is_relative() {
                source_path = std::env::current_dir().unwrap().join(source_path);
            }
            match std::fs::read_to_string(&source_path) {
                Ok(source_code) => Ok(Some(SourceResponseBody {
                    content: source_code,
                    mime_type: None,
                })),
                Err(error) => {
                    return self
                        .send_response::<()>(
                            request,
                            Err(DebuggerError::ReadSourceError {
                                source_file_name: (&source_path.to_string_lossy()).to_string(),
                                original_error: error,
                            }),
                        )
                        .is_ok()
                }
            }
        } else {
            return self
                .send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!("Unable to open resource"))),
                )
                .is_ok();
        };

        self.send_response(request, result).is_ok()
    }

    pub(crate) fn variables(&mut self, _core_data: &mut CoreData, request: &Request) -> bool {
        let arguments: VariablesArguments = match get_arguments(request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)).is_ok(),
        };
        return self
            .send_response(
                request,
                match self
                    .variable_map
                    .clone()
                    .get(&(arguments.variables_reference))
                {
                    Some(dap_variables) => {
                        match arguments.filter {
                            Some(filter) => {
                                match filter.as_str() {
                                    // TODO: Use `probe_rs::Variables` for the `variable_map`, and then transform them here before serving them up.
                                    // That way we can actually track indexed versus named variables (The DAP protocol doesn't have Variable fields to do so).
                                    "indexed" => Ok(Some(VariablesResponseBody {
                                        variables: dap_variables.clone(),
                                    })),
                                    "named" => Ok(Some(VariablesResponseBody {
                                        variables: dap_variables.clone(),
                                    })),
                                    other => Err(DebuggerError::Other(anyhow!(
                                        "ERROR: Received invalid variable filter: {}",
                                        other
                                    ))),
                                }
                            }
                            None => Ok(Some(VariablesResponseBody {
                                variables: dap_variables.clone(),
                            })),
                        }
                    }
                    None => Err(DebuggerError::Other(anyhow!(
                        "No variable information found!"
                    ))),
                },
            )
            .is_ok();
    }

    pub(crate) fn r#continue(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        match core_data.target_core.run() {
            Ok(_) => {
                self.last_known_status = core_data
                    .target_core
                    .status()
                    .unwrap_or(CoreStatus::Unknown);
                match self.adapter_type() {
                    DebugAdapterType::CommandLine => self
                        .send_response(
                            request,
                            Ok(Some(self.last_known_status.short_long_status().1)),
                        )
                        .is_ok(),
                    DebugAdapterType::DapClient => {
                        self.send_response(
                            request,
                            Ok(Some(ContinueResponseBody {
                                all_threads_continued: if self.last_known_status
                                    == CoreStatus::Running
                                {
                                    Some(true)
                                } else {
                                    Some(false)
                                },
                            })),
                        );
                        // We have to consider the fact that sometimes the `run()` is successfull,
                        // but "immediately" after the MCU hits a breakpoint or exception.
                        // So we have to check the status again to be sure.
                        thread::sleep(Duration::from_millis(100)); // Small delay to make sure the MCU hits user breakpoints early in `main()`.
                        let core_status = match core_data.target_core.status() {
                            Ok(new_status) => match new_status {
                                CoreStatus::Halted(_) => {
                                    let event_body = Some(StoppedEventBody {
                                        reason: new_status.short_long_status().0.to_owned(),
                                        description: Some(
                                            new_status.short_long_status().1.to_string(),
                                        ),
                                        thread_id: Some(core_data.target_core.id() as i64),
                                        preserve_focus_hint: None,
                                        text: None,
                                        all_threads_stopped: Some(true),
                                        hit_breakpoint_ids: None,
                                    });
                                    self.send_event("stopped", event_body);
                                    new_status
                                }
                                other => other,
                            },
                            Err(_) => CoreStatus::Unknown,
                        };
                        self.last_known_status = core_status;
                        true
                    }
                }
            }
            Err(error) => {
                self.last_known_status = CoreStatus::Halted(HaltReason::Unknown);
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
                    .is_ok()
            }
        }
    }

    /// Steps at 'instruction' granularity ONLY.
    pub(crate) fn next(&mut self, core_data: &mut CoreData, request: &Request) -> bool {
        // TODO: Implement 'statement' granularity, then update DAP `Capabilities` and read `NextArguments`.
        // let args: NextArguments = get_arguments(&request)?;

        match core_data.target_core.step() {
            Ok(cpu_info) => {
                let new_status = match core_data.target_core.status() {
                    Ok(new_status) => new_status,
                    Err(error) => {
                        self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)));
                        return false;
                    }
                };
                self.last_known_status = new_status;
                self.send_response::<()>(request, Ok(None));
                let event_body = Some(StoppedEventBody {
                    reason: "step".to_owned(),
                    description: Some(format!(
                        "{} at address 0x{:08x}",
                        new_status.short_long_status().1,
                        cpu_info.pc
                    )),
                    thread_id: Some(core_data.target_core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(true),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body).is_ok()
            }
            Err(error) => self
                .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
                .is_ok(),
        }
    }

    /// return a newly allocated id for a register scope reference
    fn new_variable_map_key(&mut self) -> i64 {
        self.variable_map_key_seq += 1;
        self.variable_map_key_seq
    }

    /// recurse through each variable and add children with parent reference to self.variables_map
    /// returns a tuple containing the parent's  (variables_map_key, named_child_variables_cnt, indexed_child_variables_cnt)
    fn create_variable_map(&mut self, variables: &[probe_rs::debug::Variable]) -> (i64, i64, i64) {
        let mut named_child_variables_cnt = 0;
        let mut indexed_child_variables_cnt = 0;
        let dap_variables: Vec<Variable> = variables
            .iter()
            .map(|variable| {
                // TODO: The DAP Protocol doesn't seem to have an easy way to indicate if a variable is `Named` or `Indexed`.
                // Figure out what needs to be done to improve this.
                if variable.kind == VariableKind::Indexed {
                    indexed_child_variables_cnt += 1;
                } else {
                    named_child_variables_cnt += 1;
                }

                let (variables_reference, named_variables_cnt, indexed_variables_cnt) =
                    match &variable.children {
                        Some(children) => self.create_variable_map(children),
                        None => (0, 0, 0),
                    };
                Variable {
                    name: variable.name.clone(),
                    value: variable.get_value(),
                    type_: Some(variable.type_name.clone()),
                    presentation_hint: None,
                    evaluate_name: None,
                    variables_reference,
                    named_variables: Some(named_variables_cnt),
                    indexed_variables: Some(indexed_variables_cnt),
                    memory_reference: Some(format!("0x{:08x}", variable.memory_location)),
                }
            })
            .collect();

        if named_child_variables_cnt > 0 || indexed_child_variables_cnt > 0 {
            let variable_map_key = self.new_variable_map_key();
            match self.variable_map.insert(variable_map_key, dap_variables) {
                Some(_) => {
                    log::warn!("Failed to create a unique `variable_map_key`. Variables shown in this frame may be incomplete or corrupted. Please report this as a bug!");
                    (0, 0, 0)
                }
                None => (
                    variable_map_key,
                    named_child_variables_cnt,
                    indexed_child_variables_cnt,
                ),
            }
        } else {
            // Returning 0's allows VSCode DAP Client to behave correctly for frames that have no variables, and variables that have no children.
            (0, 0, 0)
        }
    }

    /// Returns one of the standard DAP Requests if all goes well, or a "error" request, which should indicate that the calling function should return.
    /// When preparing to return an "error" request, we will send a Response containing the DebuggerError encountered.
    pub fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.adapter.listen_for_request()
    }

    /// Sends either the success response or an error response if passed a
    /// DebuggerError. For the DAP Client, it forwards the response, while for
    /// the CLI, it will print the body for success, or the message for
    /// failure.
    pub fn send_response<S: Serialize>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, DebuggerError>,
    ) -> Result<()> {
        self.adapter.send_response(request, response)
    }

    pub fn send_error_response(&mut self, response: &DebuggerError) -> Result<()> {
        if self
            .adapter
            .show_message(MessageSeverity::Error, response.to_string())
        {
            Ok(())
        } else {
            Err(anyhow!("Failed to send error response"))
        }
    }

    pub fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> Result<()> {
        self.adapter.send_event(event_type, event_body)
    }

    pub fn log_to_console<S: Into<String>>(&mut self, message: S) -> bool {
        self.adapter.log_to_console(message)

        /*
        if self.adapter_type == DebugAdapterType::DapClient {
            let event_body = match serde_json::to_value(OutputEventBody {
                output: format!("{}\n", message.into()),
                category: Some("console".to_owned()),
                variables_reference: None,
                source: None,
                line: None,
                column: None,
                data: None,
                group: Some("probe-rs-debug".to_owned()),
            }) {
                Ok(event_body) => event_body,
                Err(_) => {
                    return false;
                }
            };
            self.send_event("output", Some(event_body))
        } else {
            println!("{}", message.into());
            true
        }
        */
    }

    /// Send a custom "probe-rs-show-message" event to the MS DAP Client.
    /// The `severity` field can be one of `information`, `warning`, or `error`.
    pub fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool {
        self.adapter.show_message(severity, message)
    }

    /// Send a custom `probe-rs-rtt-channel-config` event to the MS DAP Client, to create a window for a specific RTT channel.
    pub fn rtt_window(
        &mut self,
        channel_number: usize,
        channel_name: String,
        data_format: DataFormat,
    ) -> bool {
        if self.adapter_type() == DebugAdapterType::DapClient {
            let event_body = match serde_json::to_value(RttChannelEventBody {
                channel_number,
                channel_name,
                data_format,
            }) {
                Ok(event_body) => event_body,
                Err(_) => {
                    return false;
                }
            };
            self.send_event("probe-rs-rtt-channel-config", Some(event_body))
                .is_ok()
        } else {
            true
        }
    }

    /// Send a custom `probe-rs-rtt-data` event to the MS DAP Client, to
    pub fn rtt_output(&mut self, channel_number: usize, rtt_data: String) -> bool {
        if self.adapter_type() == DebugAdapterType::DapClient {
            let event_body = match serde_json::to_value(RttDataEventBody {
                channel_number,
                data: rtt_data,
            }) {
                Ok(event_body) => event_body,
                Err(_) => {
                    return false;
                }
            };
            self.send_event("probe-rs-rtt-data", Some(event_body))
                .is_ok()
        } else {
            println!("RTT Channel {}: {}", channel_number, rtt_data);
            true
        }
    }

    fn new_progress_id(&mut self) -> ProgressId {
        let id = self.progress_id;

        self.progress_id += 1;

        id
    }

    pub fn start_progress(&mut self, title: &str, request_id: Option<i64>) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        let progress_id = self.new_progress_id();

        self.send_event(
            "progressStart",
            Some(ProgressStartEventBody {
                cancellable: Some(false),
                message: None,
                percentage: None,
                progress_id: progress_id.to_string(),
                request_id,
                title: title.to_owned(),
            }),
        )?;

        Ok(progress_id)
    }

    pub fn end_progress(&mut self, progress_id: ProgressId) -> Result<()> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        self.send_event(
            "progressEnd",
            Some(ProgressEndEventBody {
                message: None,
                progress_id: progress_id.to_string(),
            }),
        )
    }

    pub(crate) fn set_console_log_level(&mut self, error: ConsoleLog) {
        self.adapter.set_console_log_level(error)
    }
}

/// Provides halt functionality that is re-used elsewhere, in context of multiple DAP Requests
pub(crate) fn halt_core(
    target_core: &mut probe_rs::Core,
) -> Result<probe_rs::CoreInformation, DebuggerError> {
    match target_core.halt(Duration::from_millis(100)) {
        Ok(cpu_info) => Ok(cpu_info),
        Err(error) => Err(DebuggerError::Other(anyhow!("{}", error))),
    }
}

pub fn get_arguments<T: DeserializeOwned>(req: &Request) -> Result<T, crate::DebuggerError> {
    let value = req
        .arguments
        .as_ref()
        .ok_or(crate::DebuggerError::InvalidRequest)?;

    serde_json::from_value(value.to_owned()).map_err(|e| e.into())
}

pub(crate) trait DapStatus {
    fn short_long_status(&self) -> (&'static str, &'static str);
}
impl DapStatus for CoreStatus {
    /// Return a tuple with short and long descriptions of the core status for human machine interface / hmi. The short status matches with the strings implemented by the Microsoft DAP protocol, e.g. `let (short_status, long status) = CoreStatus::short_long_status(core_status)`
    fn short_long_status(&self) -> (&'static str, &'static str) {
        match self {
            CoreStatus::Running => ("continued", "Core is running"),
            CoreStatus::Sleeping => ("sleeping", "Core is in SLEEP mode"),
            CoreStatus::LockedUp => (
                "lockedup",
                "Core is in LOCKUP status - encountered an unrecoverable exception",
            ),
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Breakpoint => (
                    "breakpoint",
                    "Core halted due to a breakpoint (software or hardware)",
                ),
                HaltReason::Exception => (
                    "exception",
                    "Core halted due to an exception, e.g. interupt handler",
                ),
                HaltReason::Watchpoint => (
                    "data breakpoint",
                    "Core halted due to a watchpoint or data breakpoint",
                ),
                HaltReason::Step => ("step", "Core halted after a 'step' instruction"),
                HaltReason::Request => (
                    "pause",
                    "Core halted due to a user (debugger client) request",
                ),
                HaltReason::External => ("external", "Core halted due to an external request"),
                _other => ("unrecognized", "Core halted: unrecognized cause"),
            },
            CoreStatus::Unknown => ("unknown", "Core status cannot be determined"),
        }
    }
}
