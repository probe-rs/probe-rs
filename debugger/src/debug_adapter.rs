use crate::dap_types;
use crate::debugger::ConsoleLog;
use crate::debugger::CoreData;
use crate::DebuggerError;
use anyhow::{anyhow, Result};
use dap_types::*;
use parse_int::parse;
use probe_rs::debug::{VariableCache, VariableName};
use probe_rs::{debug::ColumnType, CoreStatus, HaltReason, MemoryInterface};
use probe_rs_cli_util::rtt;
use serde::{de::DeserializeOwned, Serialize};
use std::string::ToString;
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
    progress_id: ProgressId,
    /// Flag to indicate if the connected client supports progress reporting.
    pub(crate) supports_progress_reporting: bool,
    /// Flags to improve breakpoint accuracy.
    /// [DWARF] spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) lines_start_at_1: bool,
    /// [DWARF] spec at Sect 2.14 uses 1 based numbering, with a 0 indicating not-specified. We will follow that standard, and translate incoming requests depending on the DAP Client treatment of 0 or 1 based numbering.
    pub(crate) columns_start_at_1: bool,
    adapter: P,
}

impl<P: ProtocolAdapter> DebugAdapter<P> {
    pub fn new(adapter: P) -> DebugAdapter<P> {
        DebugAdapter {
            last_known_status: CoreStatus::Unknown,
            halt_after_reset: false,
            progress_id: 0,
            supports_progress_reporting: false,
            lines_start_at_1: true,
            columns_start_at_1: true,
            adapter,
        }
    }

    pub(crate) fn adapter_type(&self) -> DebugAdapterType {
        P::ADAPTER_TYPE
    }

    pub(crate) fn status(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let status = match core_data.target_core.status() {
            Ok(status) => {
                self.last_known_status = status;
                status
            }
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read core status. {:?}",
                        error
                    ))),
                )
            }
        };
        if status.is_halted() {
            let pc = core_data
                .target_core
                .read_core_reg(core_data.target_core.registers().program_counter());
            match pc {
                Ok(pc) => self.send_response(
                    request,
                    Ok(Some(format!(
                        "Status: {:?} at address {:#010x}",
                        status.short_long_status().1,
                        pc
                    ))),
                ),

                Err(error) => self
                    .send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error)))),
            }
        } else {
            self.send_response(request, Ok(Some(status.short_long_status().1.to_string())))
        }
    }

    pub(crate) fn pause(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
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
                self.send_event("stopped", event_body)?;
                self.send_response(
                    request,
                    Ok(Some(format!(
                        "Core stopped at address {:#010x}",
                        cpu_info.pc
                    ))),
                )?;
                self.last_known_status = CoreStatus::Halted(HaltReason::Request);

                Ok(())
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
            }
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

    pub(crate) fn read_memory(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: ReadMemoryArguments = match self.adapter_type() {
            DebugAdapterType::CommandLine => match request.arguments.as_ref().unwrap().try_into() {
                Ok(arguments) => arguments,
                Err(error) => return self.send_response::<()>(request, Err(error)),
            },
            DebugAdapterType::DapClient => match get_arguments(&request) {
                Ok(arguments) => arguments,
                Err(error) => return self.send_response::<()>(request, Err(error)),
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
                    format!("{:#010x} = {:#010x}\n", address + (offset * 4) as u32, word).as_str(),
                );
            }
            self.send_response::<String>(request, Ok(Some(response)))
        } else {
            self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!(
                    "Could not read any data at address {:#010x}",
                    address
                ))),
            )
        }
    }
    pub(crate) fn write(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };
        let data = match get_int_argument(request.arguments.as_ref(), "data", 1) {
            Ok(data) => data,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        match core_data
            .target_core
            .write_word_32(address, data)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => Ok(()),
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }
    pub(crate) fn set_breakpoint(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        match core_data
            .target_core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => {
                return self.send_response(
                    request,
                    Ok(Some(format!(
                        "Set new breakpoint at address {:#08x}",
                        address
                    ))),
                );
            }
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }
    pub(crate) fn clear_breakpoint(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let address = match get_int_argument(request.arguments.as_ref(), "address", 0) {
            Ok(address) => address,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        match core_data
            .target_core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)
        {
            Ok(_) => Ok(()),
            Err(error) => self.send_response::<()>(request, Err(error)),
        }
    }

    pub(crate) fn show_cpu_register_values(
        &mut self,
        _core_data: &mut CoreData,
        _request: &Request,
    ) -> Result<()> {
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
    ) -> Result<()> {
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
    pub(crate) fn restart(
        &mut self,
        core_data: &mut CoreData,
        request: Option<Request>,
    ) -> Result<()> {
        match core_data.target_core.halt(Duration::from_millis(500)) {
            Ok(_) => {}
            Err(error) => {
                if let Some(request) = request {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!("{}", error))),
                    );
                } else {
                    return self.send_error_response(&DebuggerError::Other(anyhow!("{}", error)));
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

                    self.send_event("continued", event_body)
                }
                Err(error) => {
                    return self.send_response::<()>(
                        request.unwrap(), // Checked above
                        Err(DebuggerError::Other(anyhow!("{}", error))),
                    );
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
                                return self.send_response::<()>(request, Ok(None));
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
                        self.send_event("stopped", event_body)?;
                        self.last_known_status = CoreStatus::Halted(HaltReason::External);
                    }
                    Ok(())
                }
                Err(error) => {
                    if let Some(request) = request {
                        return self.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("{}", error))),
                        );
                    } else {
                        return self
                            .send_error_response(&DebuggerError::Other(anyhow!("{}", error)));
                    }
                }
            }
        } else {
            Ok(())
        }
    }

    pub(crate) fn configuration_done(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        // Make sure the DAP Client and the DAP Server are in sync with the status of the core.
        match core_data.target_core.status() {
            Ok(core_status) => {
                self.last_known_status = core_status;
                if core_status.is_halted() {
                    if self.halt_after_reset
                        || core_status == CoreStatus::Halted(HaltReason::Breakpoint)
                    {
                        self.send_response::<()>(request, Ok(None))?;

                        let event_body = Some(StoppedEventBody {
                            reason: core_status.short_long_status().0.to_owned(),
                            description: Some(core_status.short_long_status().1.to_string()),
                            thread_id: Some(core_data.target_core.id() as i64),
                            preserve_focus_hint: None,
                            text: None,
                            all_threads_stopped: Some(true),
                            hit_breakpoint_ids: None,
                        });
                        self.send_event("stopped", event_body)
                    } else {
                        self.r#continue(core_data, request)
                    }
                } else {
                    self.send_response::<()>(request, Ok(None))
                }
            }
            Err(error) => {
                self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read core status to synchronize the client and the probe. {:?}",
                        error
                    ))),
                )?;
                Err(anyhow!("Failed to get core status."))
            }
        }
    }
    pub(crate) fn threads(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // TODO: Implement actual thread resolution. For now, we just use the core id as the thread id.

        let single_thread = Thread {
            id: core_data.target_core.id() as i64,
            name: core_data.target_name.clone(),
        };

        let threads = vec![single_thread];
        self.send_response(request, Ok(Some(ThreadsResponseBody { threads })))
    }

    pub(crate) fn set_breakpoints(
        &mut self,
        core_data: &mut CoreData,
        request: Request,
    ) -> Result<()> {
        let args: SetBreakpointsArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Could not read arguments : {}",
                        error
                    ))),
                )
            }
        };

        let mut created_breakpoints: Vec<Breakpoint> = Vec::new(); // For returning in the Response

        let source_path = args.source.path.as_ref().map(Path::new);

        // Always clear existing breakpoints before setting new ones. The DAP Specification doesn't make allowances for deleting and setting individual breakpoints.
        match core_data.target_core.clear_all_hw_breakpoints() {
            Ok(_) => {}
            Err(error) => {
                return self.send_response::<()>(
                    request,
                    Err(DebuggerError::Other(anyhow!(
                        "Failed to clear existing breakpoints before setting new ones : {}",
                        error
                    ))),
                )
            }
        }

        if let Some(requested_breakpoints) = args.breakpoints.as_ref() {
            for bp in requested_breakpoints {
                // Some overrides to improve breakpoint accuracy when `DebugInfo::get_breakpoint_location()` has to select the best from multiple options
                let breakpoint_line = if self.lines_start_at_1 {
                    // If the debug client uses 1 based numbering, then we can use it as is.
                    bp.line as u64
                } else {
                    // If the debug client uses 0 based numbering, then we bump the number by 1
                    bp.line as u64 + 1
                };
                let breakpoint_column = if self.columns_start_at_1
                    && (bp.column.is_none() || bp.column.unwrap_or(0) == 0)
                {
                    // If the debug client uses 1 based numbering, then we can use it as is.
                    Some(bp.column.unwrap_or(1) as u64)
                } else {
                    // If the debug client uses 0 based numbering, then we bump the number by 1
                    Some(bp.column.unwrap_or(0) as u64 + 1)
                };

                // Try to find source code location
                let source_location: Option<u64> = core_data
                    .debug_info
                    .get_breakpoint_location(
                        source_path.unwrap(),
                        breakpoint_line,
                        breakpoint_column,
                    )
                    .unwrap_or(None);

                if let Some(location) = source_location {
                    let (verified, reason_msg) =
                        match core_data.target_core.set_hw_breakpoint(location as u32) {
                            Ok(_) => (
                                true,
                                Some(format!("Breakpoint at memory address: {:#010x}", location)),
                            ),
                            Err(err) => {
                                let message = format!(
                                "WARNING: Could not set breakpoint at memory address: {:#010x}: {}",
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
                        column: breakpoint_column.map(|c| c as i64),
                        end_column: None,
                        end_line: None,
                        id: None,
                        line: Some(breakpoint_line as i64),
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
    }

    pub(crate) fn stack_trace(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let _status = match core_data.target_core.status() {
            Ok(status) => {
                if !status.is_halted() {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Core must be halted before requesting a stack trace"
                        ))),
                    );
                }
            }
            Err(error) => {
                return self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
            }
        };

        let _arguments: StackTraceArguments = match self.adapter_type() {
            DebugAdapterType::CommandLine => StackTraceArguments {
                format: None,
                levels: None,
                start_frame: None,
                thread_id: core_data.target_core.id() as i64,
            },
            DebugAdapterType::DapClient => match get_arguments(&request) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::Other(anyhow!(
                            "Could not read arguments : {}",
                            error
                        ))),
                    )
                }
            },
        };

        let regs = core_data.target_core.registers();

        let pc = match core_data.target_core.read_core_reg(regs.program_counter()) {
            Ok(pc) => pc,
            Err(error) => {
                return self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))
            }
        };

        log::debug!("Replacing variable cache!");

        *core_data.variable_cache = VariableCache::new(core_data.target_core.id());

        let current_stackframes = core_data.debug_info.unwind(
            core_data.variable_cache,
            &mut core_data.target_core,
            u64::from(pc),
        )?;

        match self.adapter_type() {
            DebugAdapterType::CommandLine => {
                let mut body = "".to_string();
                for _stack_frame in current_stackframes {
                    // Iterate all the stack frames, so that `debug_info.variable_cache` gets populated.
                }
                body.push_str(format!("{}\n", &core_data.variable_cache).as_str());
                self.send_response(request, Ok(Some(body)))
            }
            DebugAdapterType::DapClient => {
                let mut frame_list: Vec<StackFrame> = current_stackframes
                    .iter()
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
                        let function_display_name = if frame.inlined_call_site.is_some() {
                            format!("{} #[inline]", frame.function_name)
                        } else {
                            format!("{} @{:#010x}", frame.function_name, frame.pc)
                        };
                        // TODO: Can we add more meaningful info to `module_id`, etc.
                        StackFrame {
                            id: frame.id as i64,
                            name: function_display_name,
                            source,
                            line,
                            column: column as i64,
                            end_column: None,
                            end_line: None,
                            module_id: None,
                            presentation_hint: Some("normal".to_owned()),
                            can_restart: Some(false),
                            instruction_pointer_reference: Some(format!("{:#010x}", frame.pc)),
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
                self.send_response(request, Ok(Some(body)))
            }
        }
    }
    /// Retrieve available scopes  
    /// - static scope  : Variables with `static` modifier
    /// - registers     : Currently supports core registers 0-15
    /// - local scope   : Variables defined between start of current frame, and the current pc (program counter)
    pub(crate) fn scopes(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: ScopesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let mut dap_scopes: Vec<Scope> = vec![];

        log::trace!("Getting scopes for frame {}", arguments.frame_id,);

        if let Some(stackframe_root_variable) = core_data
            .variable_cache
            .get_variable_by_key(arguments.frame_id)
        {
            if let Some(static_root_variable) =
                core_data.variable_cache.get_variable_by_name_and_parent(
                    &VariableName::StaticScope,
                    stackframe_root_variable.variable_key,
                )
            {
                let (static_variables_reference, static_named_variables, static_indexed_variables) =
                    self.get_variable_reference(&static_root_variable, core_data.variable_cache);
                dap_scopes.push(Scope {
                    line: None,
                    column: None,
                    end_column: None,
                    end_line: None,
                    expensive: true, // VSCode won't open this tree by default.
                    indexed_variables: Some(static_indexed_variables),
                    name: "Static".to_string(),
                    presentation_hint: Some("statics".to_string()),
                    named_variables: Some(static_named_variables),
                    source: None,
                    variables_reference: static_variables_reference,
                });
            };

            if let Some(register_root_variable) =
                core_data.variable_cache.get_variable_by_name_and_parent(
                    &VariableName::Registers,
                    stackframe_root_variable.variable_key,
                )
            {
                let (
                    register_variables_reference,
                    register_named_variables,
                    register_indexed_variables,
                ) = self.get_variable_reference(&register_root_variable, core_data.variable_cache);
                dap_scopes.push(Scope {
                    line: None,
                    column: None,
                    end_column: None,
                    end_line: None,
                    expensive: true, // VSCode won't open this tree by default.
                    indexed_variables: Some(register_indexed_variables),
                    name: "Registers".to_string(),
                    presentation_hint: Some("registers".to_string()),
                    named_variables: Some(register_named_variables),
                    source: None,
                    variables_reference: register_variables_reference,
                });
            };
            if let Some(locals_root_variable) =
                core_data.variable_cache.get_variable_by_name_and_parent(
                    &VariableName::LocalScope,
                    stackframe_root_variable.variable_key,
                )
            {
                let (locals_variables_reference, locals_named_variables, locals_indexed_variables) =
                    self.get_variable_reference(&locals_root_variable, core_data.variable_cache);
                dap_scopes.push(Scope {
                    line: stackframe_root_variable
                        .source_location
                        .as_ref()
                        .and_then(|location| location.line.map(|line| line as i64)),
                    column: stackframe_root_variable
                        .source_location
                        .as_ref()
                        .and_then(|l| {
                            l.column.map(|c| match c {
                                ColumnType::LeftEdge => 0,
                                ColumnType::Column(c) => c as i64,
                            })
                        }),
                    end_column: None,
                    end_line: None,
                    expensive: false, // VSCode will open this tree by default.
                    indexed_variables: Some(locals_indexed_variables),
                    name: "Variables".to_string(),
                    presentation_hint: Some("locals".to_string()),
                    named_variables: Some(locals_named_variables),
                    source: None,
                    variables_reference: locals_variables_reference,
                });
            };
        }

        self.send_response(request, Ok(Some(ScopesResponseBody { scopes: dap_scopes })))
    }

    pub(crate) fn source(&mut self, _core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: SourceArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
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
                    return self.send_response::<()>(
                        request,
                        Err(DebuggerError::ReadSourceError {
                            source_file_name: (&source_path.to_string_lossy()).to_string(),
                            original_error: error,
                        }),
                    )
                }
            }
        } else {
            return self.send_response::<()>(
                request,
                Err(DebuggerError::Other(anyhow!("Unable to open resource"))),
            );
        };

        self.send_response(request, result)
    }

    pub(crate) fn variables(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        let arguments: VariablesArguments = match get_arguments(&request) {
            Ok(arguments) => arguments,
            Err(error) => return self.send_response::<()>(request, Err(error)),
        };

        let response = {
            // During the intial stack unwind operation, if we encounter certain types of pointers as children of complex variables, they will not be auto-expanded and included in the variable cache. Please refer to the `is_pointer` member of [probe_rs::debug::Variable] for more information. If this is the case, we will store the `stack_frame_registers` as part of the variable definition, so that we can  resolve the variable and add it to the cache before continuing.
            // TODO: Use the DAP "Invalidated" event to refresh the variables for this stackframe. It will allow the UI to see updated compound values for pointer variables based on the newly resolved children.
            if let Some(parent_variable) = core_data
                .variable_cache
                .get_variable_by_key(arguments.variables_reference)
            {
                if parent_variable.referenced_node_offset.is_some() {
                    core_data.debug_info.cache_referenced_variables(
                        core_data.variable_cache,
                        &mut core_data.target_core,
                        &parent_variable,
                    )?;
                }
            }

            let dap_variables: Vec<Variable> = core_data
                .variable_cache
                .get_children(arguments.variables_reference)?
                .iter()
                // Filter out requested children, then map them as DAP variables
                .filter(|variable| match &arguments.filter {
                    Some(filter) => match filter.as_str() {
                        "indexed" => variable.is_indexed(),
                        "named" => !variable.is_indexed(),
                        other => {
                            // This will yield an empty Vec, which will result in a user facing error as well as the log below.
                            log::error!("Received invalid variable filter: {}", other);
                            false
                        }
                    },
                    None => true,
                })
                // Convert the `probe_rs::debug::Variable` to `probe_rs_debugger::dap_types::Variable`
                .map(|variable| {
                    let (
                        variables_reference,
                        named_child_variables_cnt,
                        indexed_child_variables_cnt,
                    ) = self.get_variable_reference(variable, core_data.variable_cache);
                    Variable {
                        name: variable.name.to_string(),
                        evaluate_name: None,
                        memory_reference: Some(format!("{:#010x}", variable.memory_location)),
                        indexed_variables: Some(indexed_child_variables_cnt),
                        named_variables: Some(named_child_variables_cnt),
                        presentation_hint: None,
                        type_: Some(variable.type_name.clone()),
                        value: variable.get_value(core_data.variable_cache),
                        variables_reference,
                    }
                })
                .collect();
            match dap_variables.len() {
                0 => Err(DebuggerError::Other(anyhow!(
                    "No variable information found for {}!",
                    arguments.variables_reference
                ))),
                _ => Ok(Some(VariablesResponseBody {
                    variables: dap_variables,
                })),
            }
        };

        self.send_response(request, response)
    }

    pub(crate) fn r#continue(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        match core_data.target_core.run() {
            Ok(_) => {
                self.last_known_status = core_data
                    .target_core
                    .status()
                    .unwrap_or(CoreStatus::Unknown);
                match self.adapter_type() {
                    DebugAdapterType::CommandLine => self.send_response(
                        request,
                        Ok(Some(self.last_known_status.short_long_status().1)),
                    ),
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
                        )?;
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
                                    self.send_event("stopped", event_body)?;
                                    new_status
                                }
                                other => other,
                            },
                            Err(_) => CoreStatus::Unknown,
                        };
                        self.last_known_status = core_status;
                        Ok(())
                    }
                }
            }
            Err(error) => {
                self.last_known_status = CoreStatus::Halted(HaltReason::Unknown);
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))?;
                Err(error.into())
            }
        }
    }

    /// Steps at 'instruction' granularity ONLY.
    pub(crate) fn next(&mut self, core_data: &mut CoreData, request: Request) -> Result<()> {
        // TODO: Implement 'statement' granularity, then update DAP `Capabilities` and read `NextArguments`.
        // let args: NextArguments = get_arguments(&request)?;

        match core_data.target_core.step() {
            Ok(cpu_info) => {
                let new_status = match core_data.target_core.status() {
                    Ok(new_status) => new_status,
                    Err(error) => {
                        self.send_response::<()>(request, Err(DebuggerError::ProbeRs(error)))?;
                        return Err(anyhow!("Failed to retrieve core status"));
                    }
                };
                self.last_known_status = new_status;
                self.send_response::<()>(request, Ok(None))?;
                let event_body = Some(StoppedEventBody {
                    reason: "step".to_owned(),
                    description: Some(format!(
                        "{} at address {:#010x}",
                        new_status.short_long_status().1,
                        cpu_info.pc
                    )),
                    thread_id: Some(core_data.target_core.id() as i64),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(true),
                    hit_breakpoint_ids: None,
                });
                self.send_event("stopped", event_body)
            }
            Err(error) => {
                self.send_response::<()>(request, Err(DebuggerError::Other(anyhow!("{}", error))))
            }
        }
    }

    /// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
    /// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
    /// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
    fn get_variable_reference(
        &mut self,
        parent_variable: &probe_rs::debug::Variable,
        cache: &mut VariableCache,
    ) -> (i64, i64, i64) {
        let mut named_child_variables_cnt = 0;
        let mut indexed_child_variables_cnt = 0;
        if let Ok(children) = cache.get_children(parent_variable.variable_key) {
            for child_variable in children {
                if child_variable.is_indexed() {
                    indexed_child_variables_cnt += 1;
                } else {
                    named_child_variables_cnt += 1;
                }
            }
        };

        if named_child_variables_cnt > 0 || indexed_child_variables_cnt > 0 {
            (
                parent_variable.variable_key,
                named_child_variables_cnt,
                indexed_child_variables_cnt,
            )
        } else if parent_variable.referenced_node_offset.is_some()
            && parent_variable.get_value(cache) != "()"
        {
            // We have not yet cached the children for this reference.
            // Provide DAP Client with a reference so that it will explicitly ask for children when the user expands it.
            (parent_variable.variable_key, 0, 0)
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
        request: Request,
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
        data_format: rtt::DataFormat,
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

    /// Update the progress report in VSCode.
    /// The progress has the range [0..1].
    pub fn update_progress(
        &mut self,
        progress: f64,
        message: Option<impl Into<String>>,
        progress_id: i64,
    ) -> Result<ProgressId> {
        anyhow::ensure!(
            self.supports_progress_reporting,
            "Progress reporting is not supported by client."
        );

        let _ok = self.send_event(
            "progressUpdate",
            Some(ProgressUpdateEventBody {
                message: message.map(|v| v.into()),
                percentage: Some(progress * 100.0),
                progress_id: progress_id.to_string(),
            }),
        )?;

        Ok(progress_id)
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
