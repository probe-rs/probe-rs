use crate::dap_types::*;
use crate::debug_adapter::*;
use crate::protocol::{DapAdapter, ProtocolAdapter};

use crate::DebuggerError;
use anyhow::{anyhow, Context, Result};
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::riscv::ArchMode as riscvArchMode, prelude::*,
    Capstone, Endian,
};
use probe_rs::config::TargetSelector;
use probe_rs::debug::DebugInfo;

use probe_rs::flashing::download_file_with_options;
use probe_rs::flashing::DownloadOptions;
use probe_rs::flashing::FlashProgress;
use probe_rs::flashing::Format;
use probe_rs::ProbeCreationError;
use probe_rs::{
    Core, CoreStatus, DebugProbeError, DebugProbeSelector, Permissions, Probe, Session,
    WireProtocol,
};
use probe_rs_cli_util::rtt;
use serde::Deserialize;
use std::cell::RefCell;
use std::net::Ipv4Addr;
use std::net::TcpListener;
use std::ops::Mul;
use std::rc::Rc;
use std::{
    env::{current_dir, set_current_dir},
    path::PathBuf,
    str::FromStr,
    thread,
    time::Duration,
};

fn default_console_log() -> Option<ConsoleLog> {
    Some(ConsoleLog::Error)
}

fn parse_probe_selector(src: &str) -> Result<DebugProbeSelector, String> {
    match DebugProbeSelector::from_str(src) {
        Ok(probe_selector) => Ok(probe_selector),
        Err(error) => Err(error.to_string()),
    }
}

/// The level of information to be logged to the debugger console. The DAP Client will set appropriate RUST_LOG env for 'launch' configurations,  and will pass the rust log output to the client debug console.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ConsoleLog {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl std::str::FromStr for ConsoleLog {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "error" => Ok(ConsoleLog::Error),
            "warn" => Ok(ConsoleLog::Error),
            "info" => Ok(ConsoleLog::Info),
            "debug" => Ok(ConsoleLog::Debug),
            "trace" => Ok(ConsoleLog::Trace),
            _ => Err(format!(
                "'{}' is not a valid console log level. Choose from [error, warn, info, debug, or trace].",
                s
            )),
        }
    }
}

#[derive(clap::Parser, Copy, Clone, Debug, Deserialize)]
pub(crate) enum TargetSessionType {
    AttachRequest,
    LaunchRequest,
}

impl std::str::FromStr for TargetSessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "attach" => Ok(TargetSessionType::AttachRequest),
            "launch" => Ok(TargetSessionType::LaunchRequest),
            _ => Err(format!(
                "'{}' is not a valid target session type. Can be either 'attach' or 'launch'].",
                s
            )),
        }
    }
}

/// Shared options for all commands which use a specific probe
#[derive(clap::Parser, Clone, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DebuggerOptions {
    /// Path to the requested working directory for the debugger
    #[clap(long, parse(from_os_str))]
    pub(crate) cwd: Option<PathBuf>,

    /// Binary to debug as a path. Relative to `cwd`, or fully qualified.
    #[clap(long, parse(from_os_str))]
    pub(crate) program_binary: Option<PathBuf>,

    /// The number associated with the debug probe to use. Use 'list' command to see available probes
    #[clap(
        long = "probe",
        parse(try_from_str = parse_probe_selector),
        help = "Use this flag to select a specific probe in the list.\n\
                    Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID."
    )]
    #[serde(alias = "probe")]
    pub(crate) probe_selector: Option<DebugProbeSelector>,

    /// The MCU Core to debug. Default is 0
    #[clap(long = "core-index", default_value_t)]
    #[serde(default)]
    pub(crate) core_index: usize,

    /// The target to be selected.
    #[clap(short, long)]
    pub(crate) chip: Option<String>,

    /// Protocol to use for target connection
    #[clap(short, long)]
    #[serde(rename = "wire_protocol")]
    pub(crate) protocol: Option<WireProtocol>,

    /// Protocol speed in kHz
    #[clap(short, long)]
    pub(crate) speed: Option<u32>,

    /// Assert target's reset during connect
    #[clap(long)]
    #[serde(default)]
    pub(crate) connect_under_reset: bool,

    /// Allow the chip to be fully erased
    #[structopt(long)]
    #[serde(default)]
    pub(crate) allow_erase_all: bool,

    /// IP port number to listen for incoming DAP connections, e.g. "50000"
    #[clap(long)]
    pub(crate) port: Option<u16>,

    /// Flash the target before debugging
    #[clap(long)]
    #[serde(default)]
    pub(crate) flashing_enabled: bool,

    /// Reset the target after flashing
    #[clap(long, required_if_eq("flashing-enabled", "true"))]
    #[serde(default)]
    pub(crate) reset_after_flashing: bool,

    /// Halt the target after reset
    #[clap(long)]
    #[serde(default)]
    pub(crate) halt_after_reset: bool,

    /// Do a full chip erase, versus page-by-page erase
    #[clap(long, required_if_eq("flashing-enabled", "true"))]
    #[serde(default)]
    pub(crate) full_chip_erase: bool,

    /// Restore erased bytes that will not be rewritten from ELF
    #[clap(long, required_if_eq("flashing-enabled", "true"))]
    #[serde(default)]
    pub(crate) restore_unwritten_bytes: bool,

    /// Level of information to be logged to the debugger console (Error, Info or Debug )
    #[clap(long)]
    #[serde(default = "default_console_log")]
    pub(crate) console_log_level: Option<ConsoleLog>,

    #[clap(flatten)]
    #[serde(flatten)]
    pub(crate) rtt: rtt::RttConfig,
}

impl DebuggerOptions {
    /// Validate the new cwd, or else set it from the environment.
    pub(crate) fn validate_and_update_cwd(&mut self, new_cwd: Option<PathBuf>) {
        self.cwd = match new_cwd {
            Some(temp_path) => {
                if temp_path.is_dir() {
                    Some(temp_path)
                } else if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    log::error!("Cannot use current working directory. Please check existence and permissions.");
                    None
                }
            }
            None => {
                if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    log::error!("Cannot use current working directory. Please check existence and permissions.");
                    None
                }
            }
        };
    }

    /// If the path to the programm to be debugged is relative, we join if with the cwd.
    pub(crate) fn qualify_and_update_program_binary(
        &mut self,
        new_program_binary: Option<PathBuf>,
    ) -> Result<(), DebuggerError> {
        self.program_binary = match new_program_binary {
            Some(temp_path) => {
                let mut new_path = PathBuf::new();
                if temp_path.is_relative() {
                    if let Some(cwd_path) = self.cwd.clone() {
                        new_path.push(cwd_path);
                    } else {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid value {:?} for `cwd`",
                            self.cwd
                        )));
                    }
                }
                new_path.push(temp_path);
                Some(new_path)
            }
            None => None,
        };
        Ok(())
    }
}

/// The supported breakpoint types
#[derive(Debug, PartialEq)]
pub enum BreakpointType {
    InstructionBreakpoint,
    SourceBreakpoint,
}

/// Provide the storage and methods to handle various [`BreakPointType`]
#[derive(Debug)]
pub struct ActiveBreakpoint {
    breakpoint_type: BreakpointType,
    breakpoint_address: u32,
}

/// DebugSession is designed to be similar to [probe_rs::Session], in as much that it provides handles to the [CoreData] instances for each of the available [probe_rs::Core] involved in the debug session.
/// To get access to the [CoreData] for a specific [Core], the
/// TODO: Adjust [DebuggerOptions] to allow multiple cores (and if appropriate, their binaries) to be specified.
pub struct DebugSession {
    pub(crate) session: Session,
    /// Provides ability to disassemble binary code.
    pub(crate) capstone: Capstone,
    /// [DebugSession] will manage one [DebugInfo] per [DebuggerOptions::program_binary]
    pub(crate) debug_infos: Vec<DebugInfo>,
    /// [DebugSession] will manage a `Vec<StackFrame>` per [Core]. Each core's collection of StackFrames will be recreated whenever a stacktrace is performed, using the results of [DebugInfo::unwind]
    pub(crate) stack_frames: Vec<Vec<probe_rs::debug::StackFrame>>,
    /// [DebugSession] will manage a `Vec<ActiveBreakpoint>` per [Core]. Each core's collection of ActiveBreakpoint's will be managed on demand.
    pub(crate) breakpoints: Vec<Vec<ActiveBreakpoint>>,
    /// The control structures for handling RTT in this Core of the DebugSession.
    pub(crate) rtt_connection: Option<RttConnection>,
}

impl DebugSession {
    pub(crate) fn new(debugger_options: &DebuggerOptions) -> Result<Self, DebuggerError> {
        let mut target_probe = match debugger_options.probe_selector.clone() {
            Some(selector) => Probe::open(selector.clone()).map_err(|e| match e {
                DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound) => {
                    DebuggerError::Other(anyhow!(
                        "Could not find the probe_selector specified as {:04x}:{:04x}:{:?}",
                        selector.vendor_id,
                        selector.product_id,
                        selector.serial_number
                    ))
                }
                other_error => DebuggerError::DebugProbe(other_error),
            }),
            None => {
                // Only automatically select a probe if there is only a single probe detected.
                let list = Probe::list_all();
                if list.len() > 1 {
                    return Err(DebuggerError::Other(anyhow!(
                        "Found multiple ({}) probes",
                        list.len()
                    )));
                }

                if let Some(info) = list.first() {
                    Probe::open(info).map_err(DebuggerError::DebugProbe)
                } else {
                    return Err(DebuggerError::Other(anyhow!(
                        "No probes found. Please check your USB connections."
                    )));
                }
            }
        }?;

        let target_selector = match &debugger_options.chip {
            Some(identifier) => identifier.into(),
            None => TargetSelector::Auto,
        };

        // Set the protocol, if the user explicitly selected a protocol. Otherwise, use the default protocol of the probe.
        if let Some(protocol) = debugger_options.protocol {
            target_probe.select_protocol(protocol)?;
        }

        // Set the speed.
        if let Some(speed) = debugger_options.speed {
            let actual_speed = target_probe.set_speed(speed)?;
            if actual_speed != speed {
                log::warn!(
                    "Protocol speed {} kHz not supported, actual speed is {} kHz",
                    speed,
                    actual_speed
                );
            }
        }

        let mut permissions = Permissions::new();
        if debugger_options.allow_erase_all {
            permissions = permissions.allow_erase_all();
        }

        // Attach to the probe.
        let target_session = if debugger_options.connect_under_reset {
            target_probe.attach_under_reset(target_selector, permissions)?
        } else {
            target_probe
                .attach(target_selector, permissions)
                .map_err(|err| {
                    anyhow!(
                        "Error attaching to the probe: {:?}.\nTry the --connect-under-reset option",
                        err
                    )
                })?
        };

        // Create an instance of the [`capstone::Capstone`] for disassembly capabilities.
        let capstone = match target_session.architecture() {
            probe_rs::Architecture::Arm => Capstone::new()
                .arm()
                .mode(armArchMode::Thumb)
                .endian(Endian::Little)
                .build()
                .map_err(|err| anyhow!("Error creating Capstone disassembler: {:?}", err))?,
            probe_rs::Architecture::Riscv => Capstone::new()
                .riscv()
                .mode(riscvArchMode::RiscV32)
                .endian(Endian::Little)
                .build()
                .map_err(|err| anyhow!("Error creating Capstone disassembler: {:?}", err))?,
        };

        // Change the current working directory if `debugger_options.cwd` is `Some(T)`.
        if let Some(new_cwd) = debugger_options.cwd.clone() {
            set_current_dir(new_cwd.as_path()).map_err(|err| {
                anyhow!(
                    "Failed to set current working directory to: {:?}, {:?}",
                    new_cwd,
                    err
                )
            })?;
        };

        // TODO: We currently only allow a single core & binary to be specified in [DebuggerOptions]. When this is extended to support multicore, the following should initialize [DebugInfo] and [VariableCache] for each available core.
        // Configure the [DebugInfo].
        let debug_infos = vec![
            if let Some(binary_path) = &debugger_options.program_binary {
                DebugInfo::from_file(binary_path)
                    .map_err(|error| DebuggerError::Other(anyhow!(error)))?
            } else {
                return Err(anyhow!(
                    "Please provide a valid `program_binary` for this debug session"
                )
                .into());
            },
        ];

        // Configure the [VariableCache].
        let stack_frames = target_session.list_cores()
            .iter()
            .map(|(core_id, core_type)| {
                log::debug!(
                    "Preparing stack frame variable cache for DebugSession and CoreData for core #{} of type: {:?}",
                    core_id,
                    core_type
                );
                Vec::<probe_rs::debug::StackFrame>::new()
            }).collect();

        // Prepare the breakpoint cache
        let breakpoints = target_session.list_cores()
            .iter()
            .map(|(core_id, core_type)| {
                log::debug!(
                    "Preparing breakpoint cache for DebugSession and CoreData for core #{} of type: {:?}",
                    core_id,
                    core_type
                );
                Vec::<ActiveBreakpoint>::new()
            }).collect();

        Ok(DebugSession {
            session: target_session,
            capstone,
            debug_infos,
            stack_frames,
            breakpoints,
            rtt_connection: None,
        })
    }

    pub fn attach_core(&mut self, core_index: usize) -> Result<CoreData, DebuggerError> {
        let target_name = self.session.target().name.clone();
        // Do a 'light weight'(just get references to existing data structures) attach to the core and return relevant debug data.
        match self.session.core(core_index) {
            Ok(target_core) => Ok(CoreData {
                target_core,
                target_name: format!("{}-{}", core_index, target_name),
                debug_info: self.debug_infos.get(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "No available `DebugInfo` for core # {}",
                        core_index
                    ))
                })?,
                stack_frames: self.stack_frames.get_mut(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "StackFrame cache was not correctly configured for core # {}",
                        core_index
                    ))
                })?,
                capstone: &self.capstone,
                breakpoints: self.breakpoints.get_mut(core_index).ok_or_else(|| {
                    DebuggerError::Other(anyhow!(
                        "ActiveBreakpoint cache was not correctly configured for core # {}",
                        core_index
                    ))
                })?,
                rtt_connection: &mut self.rtt_connection,
            }),
            Err(_) => Err(DebuggerError::UnableToOpenProbe(Some(
                "No core at the specified index.",
            ))),
        }
    }
}

/// Manage the active RTT target for a specific DebugSession, as well as provide methods to reliably move RTT from target, through the debug_adapter, to the client.
pub(crate) struct RttConnection {
    /// The connection to RTT on the target
    target_rtt: rtt::RttActiveTarget,
    /// Some status fields and methods to ensure continuity in flow of data from target to debugger to client.
    debugger_rtt_channels: Vec<DebuggerRttChannel>,
}

impl RttConnection {
    /// Polls all the available channels for data and transmits data to the client.
    /// If at least one channel had data, then return a `true` status.
    pub fn process_rtt_data<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        target_core: &mut Core,
    ) -> bool {
        let mut at_least_one_channel_had_data = false;
        for debugger_rtt_channel in self.debugger_rtt_channels.iter_mut() {
            if debugger_rtt_channel.poll_rtt_data(target_core, debug_adapter, &mut self.target_rtt)
            {
                at_least_one_channel_had_data = true;
            }
        }
        at_least_one_channel_had_data
    }
}

pub(crate) struct DebuggerRttChannel {
    pub(crate) channel_number: usize,
    // We will not poll target RTT channels until we have confirmation from the client that the output window has been opened.
    pub(crate) has_client_window: bool,
}
impl DebuggerRttChannel {
    /// Poll and retrieve data from the target, and send it to the client, depending on the state of `hasClientWindow`.
    /// Doing this selectively ensures that we don't pull data from target buffers until we have an output window, and also helps us drain buffers after the target has entered a `is_halted` state.
    /// Errors will be reported back to the `debug_adapter`, and the return `bool` value indicates whether there was available data that was processed.
    pub(crate) fn poll_rtt_data<P: ProtocolAdapter>(
        &mut self,
        core: &mut Core,
        debug_adapter: &mut DebugAdapter<P>,
        rtt_target: &mut rtt::RttActiveTarget,
    ) -> bool {
        if self.has_client_window {
            rtt_target
                .active_channels
                .iter_mut()
                .find(|active_channel| {
                    if let Some(channel_number) = active_channel.number() {
                        channel_number == self.channel_number
                    } else {
                        false
                    }
                })
                .and_then(|rtt_channel| {
                    rtt_channel.get_rtt_data(core, rtt_target.defmt_state.as_ref())
                })
                .and_then(|(channel_number, channel_data)| {
                    if debug_adapter
                        .rtt_output(channel_number.parse::<usize>().unwrap_or(0), channel_data)
                    {
                        Some(true)
                    } else {
                        None
                    }
                })
                .is_some()
        } else {
            false
        }
    }
}

/// [CoreData] provides handles to various data structures required to debug a single instance of a core. The actual state is stored in [SessionData].
///
/// Usage: To get access to this structure please use the [DebugSession::attach_core] method. Please keep access/locks to this to a minumum duration.
pub struct CoreData<'p> {
    pub(crate) target_core: Core<'p>,
    pub(crate) target_name: String,
    pub(crate) debug_info: &'p DebugInfo,
    pub(crate) stack_frames: &'p mut Vec<probe_rs::debug::StackFrame>,
    pub(crate) capstone: &'p Capstone,
    pub(crate) breakpoints: &'p mut Vec<ActiveBreakpoint>,
    pub(crate) rtt_connection: &'p mut Option<RttConnection>,
}

impl<'p> CoreData<'p> {
    /// Search available [StackFrame]'s for the given `id`
    pub(crate) fn get_stackframe(&'p self, id: i64) -> Option<&'p probe_rs::debug::StackFrame> {
        self.stack_frames
            .iter()
            .find(|stack_frame| stack_frame.id == id)
    }

    /// Confirm RTT initialization on the target, and use the RTT channel configurations to initialize the output windows on the DAP Client.
    pub fn attach_to_rtt<P: ProtocolAdapter>(
        &mut self,
        debug_adapter: &mut DebugAdapter<P>,
        target_memory_map: &[probe_rs::config::MemoryRegion],
        program_binary: &std::path::Path,
        rtt_config: &rtt::RttConfig,
    ) -> Result<()> {
        let mut debugger_rtt_channels: Vec<DebuggerRttChannel> = vec![];
        match rtt::attach_to_rtt(
            &mut self.target_core,
            target_memory_map,
            program_binary,
            rtt_config,
        ) {
            Ok(target_rtt) => {
                for any_channel in target_rtt.active_channels.iter() {
                    if let Some(up_channel) = &any_channel.up_channel {
                        debugger_rtt_channels.push(DebuggerRttChannel {
                            channel_number: up_channel.number(),
                            // This value will eventually be set to true by a VSCode client request "rttWindowOpened"
                            has_client_window: false,
                        });
                        debug_adapter.rtt_window(
                            up_channel.number(),
                            any_channel.channel_name.clone(),
                            any_channel.data_format,
                        );
                    }
                }
                *self.rtt_connection = Some(RttConnection {
                    target_rtt,
                    debugger_rtt_channels,
                });
            }
            Err(_error) => {
                log::warn!("Failed to initalize RTT. Will try again on the next request... ");
            }
        };
        Ok(())
    }

    /// Set a single breakpoint in target configuration as well as [`CoreData::breakpoints`]
    pub(crate) fn set_breakpoint(
        &mut self,
        address: u32,
        breakpoint_type: BreakpointType,
    ) -> Result<(), DebuggerError> {
        self.target_core
            .set_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        self.breakpoints.push(ActiveBreakpoint {
            breakpoint_type,
            breakpoint_address: address,
        });
        Ok(())
    }

    /// Clear a single breakpoint from target configuration as well as [`CoreData::breakpoints`]
    pub(crate) fn clear_breakpoint(&mut self, address: u32) -> Result<()> {
        self.target_core
            .clear_hw_breakpoint(address)
            .map_err(DebuggerError::ProbeRs)?;
        let mut breakpoint_position: Option<usize> = None;
        for (position, active_breakpoint) in self.breakpoints.iter().enumerate() {
            if active_breakpoint.breakpoint_address == address {
                breakpoint_position = Some(position);
                break;
            }
        }
        if let Some(breakpoint_position) = breakpoint_position {
            self.breakpoints.remove(breakpoint_position as usize);
        }
        Ok(())
    }

    /// Clear all breakpoints of a specified [`BreakpointType`]. Affects target configuration as well as [`CoreData::breakpoints`]
    pub(crate) fn clear_breakpoints(&mut self, breakpoint_type: BreakpointType) -> Result<()> {
        let target_breakpoints = self
            .breakpoints
            .iter()
            .filter(|breakpoint| breakpoint.breakpoint_type == breakpoint_type)
            .map(|breakpoint| breakpoint.breakpoint_address)
            .collect::<Vec<u32>>();
        for breakpoint in target_breakpoints {
            self.clear_breakpoint(breakpoint).ok();
        }
        Ok(())
    }
}

#[derive(Debug)]
/// The `DebuggerStatus` is used to control how the Debugger::debug_session() decides if it should respond to DAP Client requests such as `Terminate`, `Disconnect`, and `Reset`, as well as how to repond to unrecoverable errors during a debug session interacting with a target session.
pub(crate) enum DebuggerStatus {
    ContinueSession,
    TerminateSession,
}

/// #Debugger Overview
/// The DAP Server will usually be managed automatically by the VSCode client.
/// The DAP Server can optionally be run from the command line as a "server" process.
/// - In this case, the management (start and stop) of the server process is the responsibility of the user. e.g.
///   - `probe-rs-debug --debug --port <IP port number> <other options>` : Uses TCP Sockets to the defined IP port number to service DAP requests.
pub struct Debugger {
    debugger_options: DebuggerOptions,
}

impl Debugger {
    pub fn new(debugger_options: DebuggerOptions) -> Self {
        // Define all the commands supported by the debugger.
        // TODO: There is a lot of repetitive code here, and a great opportunity for macros.
        Self { debugger_options }
    }

    pub(crate) fn process_next_request<P: ProtocolAdapter>(
        &mut self,
        session_data: &mut DebugSession,
        debug_adapter: &mut DebugAdapter<P>,
    ) -> Result<DebuggerStatus, DebuggerError> {
        let request = debug_adapter.listen_for_request()?;
        match request {
            None => {
                /*
                The logic of this command is as follows:
                - While we are waiting for DAP-Client (TCP or STDIO), we have to continuously check in on the status of the probe.
                - Initally, while `LAST_KNOWN_STATUS` probe-rs::CoreStatus::Unknown, we do nothing. Wait until latter part of `debug_session` sets it to something known.
                - If the `LAST_KNOWN_STATUS` is `Halted`, then we stop polling the Probe until the next DAP-Client request attempts an action
                - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
                - If the `new_status` is different from the `LAST_KNOWN_STATUS`, then we have to tell the DAP-Client by way of an `Event`
                - If the `new_status` is `Running`, then we have to poll on a regular basis, until the Probe stops for good reasons like breakpoints, or bad reasons like panics. Then tell the DAP-Client.
                */
                match debug_adapter.last_known_status {
                    CoreStatus::Unknown => Ok(DebuggerStatus::ContinueSession), // Don't do anything until we know VSCode's startup sequence is complete, and changes this to either Halted or Running.
                    CoreStatus::Halted(_) => {
                        // Make sure the RTT buffers are drained.
                        match session_data.attach_core(self.debugger_options.core_index) {
                            Ok(mut core_data) => {
                                if let Some(rtt_active_target) = &mut core_data.rtt_connection {
                                    rtt_active_target.process_rtt_data(
                                        debug_adapter,
                                        &mut core_data.target_core,
                                    );
                                };
                            }
                            Err(error) => {
                                let _ = debug_adapter.send_error_response(&error)?;
                                return Err(error);
                            }
                        };

                        // No need to poll the target status if we know it is halted and waiting for us to do something.
                        thread::sleep(Duration::from_millis(50)); // Small delay to reduce fast looping costs on the client
                        Ok(DebuggerStatus::ContinueSession)
                    }
                    _other => {
                        let mut received_rtt_data = false;
                        let mut core_data =
                            match session_data.attach_core(self.debugger_options.core_index) {
                                Ok(mut core_data) => {
                                    // Use every opportunity to poll the RTT channels for data
                                    if let Some(rtt_active_target) = &mut core_data.rtt_connection {
                                        received_rtt_data = rtt_active_target.process_rtt_data(
                                            debug_adapter,
                                            &mut core_data.target_core,
                                        );
                                    }
                                    core_data
                                }
                                Err(error) => {
                                    let _ = debug_adapter.send_error_response(&error)?;
                                    return Err(error);
                                }
                            };

                        // Check and update the core status.
                        let new_status = match core_data.target_core.status() {
                            Ok(new_status) => new_status,
                            Err(error) => {
                                let error = DebuggerError::ProbeRs(error);
                                let _ = debug_adapter.send_error_response(&error);
                                return Err(error);
                            }
                        };

                        // Only sleep (nap for a short duration) IF the probe's status hasn't changed AND there was no RTT data in the last poll.
                        // Otherwise loop again to keep things flowing as fast as possible.
                        // The justification is that any client side CPU used to keep polling is a small price to pay for maximum throughput of debug requests and RTT from the probe.
                        if received_rtt_data && new_status == debug_adapter.last_known_status {
                            return Ok(DebuggerStatus::ContinueSession);
                        } else if new_status == debug_adapter.last_known_status {
                            thread::sleep(Duration::from_millis(50)); // Small delay to reduce fast looping costs.
                            return Ok(DebuggerStatus::ContinueSession);
                        } else {
                            debug_adapter.last_known_status = new_status;
                        };

                        match new_status {
                            CoreStatus::Running | CoreStatus::Sleeping => {
                                let event_body = Some(ContinuedEventBody {
                                    all_threads_continued: Some(true),
                                    thread_id: core_data.target_core.id() as i64,
                                });
                                debug_adapter.send_event("continued", event_body)?;
                            }
                            CoreStatus::Halted(_) => {
                                let event_body = Some(StoppedEventBody {
                                    reason: new_status.short_long_status().0.to_owned(),
                                    description: Some(new_status.short_long_status().1.to_owned()),
                                    thread_id: Some(core_data.target_core.id() as i64),
                                    preserve_focus_hint: Some(false),
                                    text: None,
                                    all_threads_stopped: Some(true),
                                    hit_breakpoint_ids: None,
                                });
                                debug_adapter.send_event("stopped", event_body)?;
                            }
                            CoreStatus::LockedUp => {
                                debug_adapter.show_message(
                                    MessageSeverity::Error,
                                    new_status.short_long_status().1.to_owned(),
                                );
                                return Err(DebuggerError::Other(anyhow!(new_status
                                    .short_long_status()
                                    .1
                                    .to_owned())));
                            }
                            CoreStatus::Unknown => {
                                debug_adapter.send_error_response(&DebuggerError::Other(
                                    anyhow!("Unknown Device status reveived from Probe-rs"),
                                ))?;

                                return Err(DebuggerError::Other(anyhow!(
                                    "Unknown Device status reveived from Probe-rs"
                                )));
                            }
                        };
                        Ok(DebuggerStatus::ContinueSession)
                    }
                }
            }
            Some(request) => {
                // First, attach to the core.
                let mut core_data = match session_data.attach_core(self.debugger_options.core_index)
                {
                    Ok(core_data) => core_data,
                    Err(error) => {
                        let failed_command = request.command.clone();
                        debug_adapter.send_response::<()>(request, Err(error))?;
                        return Err(DebuggerError::Other(anyhow!(
                            "Error while attaching to core. Could not complete command {}",
                            failed_command
                        )));
                    }
                };

                // For some operations, we need to make sure the core isn't sleeping, by calling `Core::halt()`.
                // When we do this, we need to flag it (`unhalt_me = true`), and later call `Core::run()` again.
                // NOTE: The target will exit sleep mode as a result of this command.
                let mut unhalt_me = false;
                match request.command.as_ref() {
                    "configurationDone"
                    | "setBreakpoint"
                    | "setBreakpoints"
                    | "setInstructionBreakpoints"
                    | "clearBreakpoint"
                    | "stackTrace"
                    | "threads"
                    | "scopes"
                    | "variables"
                    | "readMemory"
                    | "writeMemory"
                    | "disassemble" => {
                        match core_data.target_core.status() {
                            Ok(current_status) => {
                                if current_status == CoreStatus::Sleeping {
                                    match core_data.target_core.halt(Duration::from_millis(100)) {
                                        Ok(_) => {
                                            debug_adapter.last_known_status =
                                                CoreStatus::Halted(probe_rs::HaltReason::Request);
                                            unhalt_me = true;
                                        }
                                        Err(error) => {
                                            debug_adapter.send_response::<()>(
                                                request,
                                                Err(DebuggerError::Other(anyhow!("{}", error))),
                                            )?;
                                            return Err(error.into());
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                let failed_command = request.command.clone();
                                let wrapped_err = DebuggerError::ProbeRs(error);
                                debug_adapter.send_response::<()>(request, Err(wrapped_err))?;

                                // TODO: Nicer response here
                                return Err(DebuggerError::Other(anyhow!(
                                    "Failed to get core status. Could not complete command: {:?}",
                                    failed_command
                                )));
                            }
                        }
                    }
                    _ => {}
                }

                // Now we are ready to execute supported commands, or return an error if it isn't supported.
                match match request.command.clone().as_ref() {
                    "rttWindowOpened" => {
                        if let Some(debugger_rtt_target) = core_data.rtt_connection {
                            match get_arguments::<RttWindowOpenedArguments>(&request) {
                                Ok(arguments) => {
                                    debugger_rtt_target
                                        .debugger_rtt_channels
                                        .iter_mut()
                                        .find(|debugger_rtt_channel| {
                                            debugger_rtt_channel.channel_number
                                                == arguments.channel_number
                                        })
                                        .map_or(false, |rtt_channel| {
                                            rtt_channel.has_client_window =
                                                arguments.window_is_open;
                                            arguments.window_is_open
                                        });
                                    debug_adapter.send_response::<()>(request, Ok(None))?;
                                }
                                Err(error) => {
                                    debug_adapter.send_response::<()>(
                                        request,
                                        Err(DebuggerError::Other(anyhow!(
                                    "Could not deserialize arguments for RttWindowOpened : {:?}.",
                                    error
                                ))),
                                    )?;
                                }
                            }
                        }
                        Ok(DebuggerStatus::ContinueSession)
                    }
                    "disconnect" => debug_adapter
                        .send_response::<()>(request, Ok(None))
                        .and(Ok(DebuggerStatus::TerminateSession)),
                    "terminate" => debug_adapter
                        .pause(&mut core_data, request)
                        .and(Ok(DebuggerStatus::TerminateSession)),
                    "status" => debug_adapter
                        .status(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "next" => debug_adapter
                        .next(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stepIn" => debug_adapter
                        .step_in(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stepOut" => debug_adapter
                        .step_out(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "pause" => debug_adapter
                        .pause(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "readMemory" => debug_adapter
                        .read_memory(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "writeMemory" => debug_adapter
                        .write_memory(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "setVariable" => debug_adapter
                        .set_variable(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "configurationDone" => debug_adapter
                        .configuration_done(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "threads" => debug_adapter
                        .threads(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "restart" => {
                        // Reset RTT so that the link can be re-established
                        *core_data.rtt_connection = None;
                        debug_adapter
                            .restart(&mut core_data, Some(request))
                            .and(Ok(DebuggerStatus::ContinueSession))
                    }
                    "setBreakpoints" => debug_adapter
                        .set_breakpoints(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "setInstructionBreakpoints" => debug_adapter
                        .set_instruction_breakpoints(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "stackTrace" => debug_adapter
                        .stack_trace(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "scopes" => debug_adapter
                        .scopes(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "disassemble" => debug_adapter
                        .disassemble(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "variables" => debug_adapter
                        .variables(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "continue" => debug_adapter
                        .r#continue(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    "evaluate" => debug_adapter
                        .evaluate(&mut core_data, request)
                        .and(Ok(DebuggerStatus::ContinueSession)),
                    other_command => {
                        // Unimplemented command.
                        debug_adapter.send_response::<()>(
                            request,
                            Err(DebuggerError::Other(anyhow!("Received request '{}', which is not supported or not implemented yet", other_command))),)
                            .and(Ok(DebuggerStatus::ContinueSession))
                    }
                } {
                    Ok(debugger_status) => {
                        if unhalt_me {
                            match core_data.target_core.run() {
                                Ok(_) => debug_adapter.last_known_status = CoreStatus::Running,
                                Err(error) => {
                                    debug_adapter.send_error_response(&DebuggerError::Other(
                                        anyhow!("{}", error),
                                    ))?;
                                    return Err(error.into());
                                }
                            }
                        }
                        Ok(debugger_status)
                    }
                    Err(e) => Err(DebuggerError::Other(e.context("Error executing request."))),
                }
            }
        }
    }

    /// `debug_session` is where the primary _debug processing_ for the DAP (Debug Adapter Protocol) adapter happens.
    /// All requests are interpreted, actions taken, and responses formulated here. This function is self contained and returns nothing.
    /// The [`DebugAdapter`] takes care of _implementing the DAP Base Protocol_ and _communicating with the DAP client_ and _probe_.
    pub(crate) fn debug_session<P: ProtocolAdapter + 'static>(
        &mut self,
        mut debug_adapter: DebugAdapter<P>,
    ) -> Result<DebuggerStatus, DebuggerError> {
        // The DapClient startup process has a specific sequence.
        // Handle it here before starting a probe-rs session and looping through user generated requests.
        // Handling the initialize, and Attach/Launch requests here in this method,
        // before entering the iterative loop that processes requests through the process_request method.

        // Initialize request.
        let initialize_request = loop {
            let current_request = if let Some(request) = debug_adapter.listen_for_request()? {
                request
            } else {
                continue;
            };

            match current_request.command.as_str() {
                "initialize" => break current_request, // We have lift off.
                other => {
                    let command = other.to_string();

                    debug_adapter.send_response::<()>(
                        current_request,
                        Err(
                            anyhow!("Initial command was '{}', expected 'initialize'", command)
                                .into(),
                        ),
                    )?;
                    return Err(DebuggerError::Other(anyhow!(
                        "Initial command was '{}', expected 'initialize'",
                        command
                    )));
                }
            };
        };

        let initialize_arguments: InitializeRequestArguments = match get_arguments::<
            InitializeRequestArguments,
        >(&initialize_request)
        {
            Ok(arguments) => {
                if !(arguments.columns_start_at_1.unwrap_or(true)
                    && arguments.lines_start_at_1.unwrap_or(true))
                {
                    debug_adapter.send_response::<()>(initialize_request, Err(DebuggerError::Other(anyhow!("Unsupported Capability: Client requested column and row numbers start at 0."))))?;
                    return Err(DebuggerError::Other(anyhow!("Unsupported Capability: Client requested column and row numbers start at 0.")));
                }
                arguments
            }
            Err(error) => {
                debug_adapter.send_response::<()>(initialize_request, Err(error))?;
                return Err(DebuggerError::Other(anyhow!(
                    "Failed to get initialize arguments"
                )));
            }
        };

        if let Some(progress_support) = initialize_arguments.supports_progress_reporting {
            debug_adapter.supports_progress_reporting = progress_support;
        }

        if let Some(lines_start_at_1) = initialize_arguments.lines_start_at_1 {
            debug_adapter.lines_start_at_1 = lines_start_at_1;
        }

        if let Some(columns_start_at_1) = initialize_arguments.columns_start_at_1 {
            debug_adapter.columns_start_at_1 = columns_start_at_1;
        }

        // Reply to Initialize with `Capabilities`.
        let capabilities = Capabilities {
            supports_configuration_done_request: Some(true),
            supports_restart_request: Some(true),
            supports_terminate_request: Some(true),
            supports_delayed_stack_trace_loading: Some(true),
            supports_read_memory_request: Some(true),
            supports_write_memory_request: Some(true),
            supports_set_variable: Some(true),
            supports_clipboard_context: Some(true),
            supports_disassemble_request: Some(true),
            supports_instruction_breakpoints: Some(true),
            supports_stepping_granularity: Some(true),
            // supports_value_formatting_options: Some(true),
            // supports_function_breakpoints: Some(true),
            // TODO: Use DEMCR register to implement exception breakpoints
            // supports_exception_options: Some(true),
            // supports_exception_filter_options: Some (true),
            ..Default::default()
        };
        debug_adapter.send_response(initialize_request, Ok(Some(capabilities)))?;

        // Process either the Launch or Attach request.
        let requested_target_session_type: Option<TargetSessionType>;
        let la_request = loop {
            let current_request = if let Some(request) = debug_adapter.listen_for_request()? {
                request
            } else {
                continue;
            };

            match current_request.command.as_str() {
                "attach" => {
                    requested_target_session_type = Some(TargetSessionType::AttachRequest);
                    break current_request;
                }
                "launch" => {
                    requested_target_session_type = Some(TargetSessionType::LaunchRequest);
                    break current_request;
                }
                other => {
                    let error_msg = format!(
                        "Expected request 'launch' or 'attach', but received' {}'",
                        other
                    );

                    debug_adapter.send_response::<()>(
                        current_request,
                        Err(DebuggerError::Other(anyhow!(error_msg.clone()))),
                    )?;
                    return Err(DebuggerError::Other(anyhow!(error_msg)));
                }
            };
        };

        match get_arguments(&la_request) {
            Ok(arguments) => {
                if requested_target_session_type.is_some() {
                    self.debugger_options = DebuggerOptions { ..arguments };
                    if matches!(
                        requested_target_session_type,
                        Some(TargetSessionType::AttachRequest)
                    ) {
                        // Since VSCode doesn't do field validation checks for relationships in launch.json request types, check it here.
                        if self.debugger_options.flashing_enabled
                            || self.debugger_options.reset_after_flashing
                            || self.debugger_options.halt_after_reset
                            || self.debugger_options.full_chip_erase
                            || self.debugger_options.restore_unwritten_bytes
                        {
                            debug_adapter.send_response::<()>(
                                        la_request,
                                        Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type."))),
                                    )?;

                            return Err(DebuggerError::Other(anyhow!(
                                            "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type.")));
                        }
                    }
                }
                debug_adapter.set_console_log_level(
                    self.debugger_options
                        .console_log_level
                        .unwrap_or(ConsoleLog::Error),
                );
                // Update the `cwd` and `program_binary`.
                self.debugger_options
                    .validate_and_update_cwd(self.debugger_options.cwd.clone());
                match self
                    .debugger_options
                    .qualify_and_update_program_binary(self.debugger_options.program_binary.clone())
                {
                    Ok(_) => {}
                    Err(error) => {
                        let err = DebuggerError::Other(anyhow!(
                            "Unable to validate the program_binary path '{:?}'",
                            error
                        ));

                        debug_adapter.send_error_response(&err)?;
                        return Err(err);
                    }
                }
                match self.debugger_options.program_binary.clone() {
                    Some(program_binary) => {
                        if !program_binary.is_file() {
                            debug_adapter.send_response::<()>(
                                la_request,
                                Err(DebuggerError::Other(anyhow!(
                                    "Invalid program binary file specified '{:?}'",
                                    program_binary
                                ))),
                            )?;
                            return Err(DebuggerError::Other(anyhow!(
                                "Invalid program binary file specified '{:?}'",
                                program_binary
                            )));
                        }
                    }
                    None => {
                        debug_adapter.send_response::<()>(
                            la_request,
                            Err(DebuggerError::Other(anyhow!(
                                "Please use the --program-binary option to specify an executable"
                            ))),
                        )?;

                        return Err(DebuggerError::Other(anyhow!(
                            "Please use the --program-binary option to specify an executable"
                        )));
                    }
                }
                debug_adapter.send_response::<()>(la_request, Ok(None))?;
            }
            Err(error) => {
                let err_1 = anyhow!(

                        "Could not derive DebuggerOptions from request '{}', with arguments {:?}\n{:?} ", la_request.command, la_request.arguments, error

                        );
                let err_2 =anyhow!(

                        "Could not derive DebuggerOptions from request '{}', with arguments {:?}\n{:?} ", la_request.command, la_request.arguments, error

                        );

                debug_adapter.send_response::<()>(la_request, Err(DebuggerError::Other(err_1)))?;

                return Err(DebuggerError::Other(err_2));
            }
        };

        let mut session_data = match DebugSession::new(&self.debugger_options) {
            Ok(session_data) => session_data,
            Err(error) => {
                debug_adapter.send_error_response(&error)?;
                return Err(error);
            }
        };
        debug_adapter.halt_after_reset = self.debugger_options.halt_after_reset;

        // Do the flashing.
        {
            if self.debugger_options.flashing_enabled {
                let path_to_elf = match self.debugger_options.program_binary.clone() {
                    Some(program_binary) => program_binary,
                    None => {
                        let err = DebuggerError::Other(anyhow!(
                            "Please use the --program-binary option to specify an executable"
                        ));
                        debug_adapter.send_error_response(&err)?;
                        return Err(err);
                    }
                };
                debug_adapter.log_to_console(format!(
                    "INFO: FLASHING: Starting write of {:?} to device memory",
                    &path_to_elf
                ));

                let progress_id = debug_adapter.start_progress("Flashing device", None).ok();

                let mut download_options = DownloadOptions::default();
                download_options.keep_unwritten_bytes =
                    self.debugger_options.restore_unwritten_bytes;
                download_options.do_chip_erase = self.debugger_options.full_chip_erase;
                let flash_result = {
                    let rc_debug_adapter = Rc::new(RefCell::new(debug_adapter));
                    let rc_debug_adapter_clone = rc_debug_adapter.clone();
                    let flash_result = {
                        struct ProgressState {
                            total_page_size: usize,
                            total_sector_size: usize,
                            total_fill_size: usize,
                            page_size_done: usize,
                            sector_size_done: usize,
                            fill_size_done: usize,
                        }

                        let flash_progress = Rc::new(RefCell::new(ProgressState {
                            total_page_size: 0,
                            total_sector_size: 0,
                            total_fill_size: 0,
                            page_size_done: 0,
                            sector_size_done: 0,
                            fill_size_done: 0,
                        }));

                        let flash_progress = if let Some(id) = progress_id {
                            FlashProgress::new(move |event| {
                                let mut flash_progress = flash_progress.borrow_mut();
                                let mut debug_adapter = rc_debug_adapter_clone.borrow_mut();
                                match event {
                                    probe_rs::flashing::ProgressEvent::Initialized {
                                        flash_layout,
                                    } => {
                                        flash_progress.total_page_size = flash_layout
                                            .pages()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();

                                        flash_progress.total_sector_size = flash_layout
                                            .sectors()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();

                                        flash_progress.total_fill_size = flash_layout
                                            .fills()
                                            .iter()
                                            .map(|s| s.size() as usize)
                                            .sum();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedFilling => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Reading Old Pages ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::PageFilled {
                                        size, ..
                                    } => {
                                        flash_progress.fill_size_done += size as usize;
                                        let progress = flash_progress.fill_size_done as f64
                                            / flash_progress.total_fill_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!("Reading Old Pages ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Reading Old Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedFilling => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Reading Old Pages Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedErasing => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Erasing Sectors ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::SectorErased {
                                        size,
                                        ..
                                    } => {
                                        flash_progress.sector_size_done += size as usize;
                                        let progress = flash_progress.sector_size_done as f64
                                            / flash_progress.total_sector_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!("Erasing Sectors ({})", progress)),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Erasing Sectors Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedErasing => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Erasing Sectors Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::StartedProgramming => {
                                        debug_adapter
                                            .update_progress(0.0, Some("Programming Pages ..."), id)
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::PageProgrammed {
                                        size,
                                        ..
                                    } => {
                                        flash_progress.page_size_done += size as usize;
                                        let progress = flash_progress.page_size_done as f64
                                            / flash_progress.total_page_size as f64;
                                        debug_adapter
                                            .update_progress(
                                                progress,
                                                Some(format!(
                                                    "Programming Pages ({:02.0}%)",
                                                    progress.mul(100_f64)
                                                )),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FailedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Flashing Pages Failed!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                    probe_rs::flashing::ProgressEvent::FinishedProgramming => {
                                        debug_adapter
                                            .update_progress(
                                                1.0,
                                                Some("Flashing Pages Complete!"),
                                                id,
                                            )
                                            .ok();
                                    }
                                }
                            })
                        } else {
                            FlashProgress::new(|_event| {})
                        };
                        download_options.progress = Some(&flash_progress);
                        download_file_with_options(
                            &mut session_data.session,
                            &path_to_elf,
                            Format::Elf,
                            download_options,
                        )
                    };
                    debug_adapter = match Rc::try_unwrap(rc_debug_adapter) {
                        Ok(debug_adapter) => debug_adapter.into_inner(),
                        Err(too_many_strong_references) => {
                            let other_error = DebuggerError::Other(anyhow!("Unexpected error while dereferencing the `debug_adapter` (It has {} strong references). Please report this as a bug.", Rc::strong_count(&too_many_strong_references)));
                            return Err(other_error);
                        }
                    };

                    if let Some(id) = progress_id {
                        let _ = debug_adapter.end_progress(id);
                    }
                    flash_result
                };

                match flash_result {
                    Ok(_) => {
                        debug_adapter.log_to_console(format!(
                            "INFO: FLASHING: Completed write of {:?} to device memory",
                            &path_to_elf
                        ));
                    }
                    Err(error) => {
                        let error = DebuggerError::FileDownload(error);
                        debug_adapter.send_error_response(&error)?;
                        return Err(error);
                    }
                }
            }
        }

        // This is the first attach to the requested core. If this one works, all subsequent ones will be no-op requests for a Core reference. Do NOT hold onto this reference for the duration of the session ... that is why this code is in a block of its own.
        {
            // First, attach to the core
            let mut core_data = match session_data.attach_core(self.debugger_options.core_index) {
                Ok(mut core_data) => {
                    // Immediately after attaching, halt the core, so that we can finish initalization without bumping into user code.
                    // Depending on supplied `debugger_options`, the core will be restarted at the end of initialization in the `configuration_done` request.
                    match halt_core(&mut core_data.target_core) {
                        Ok(_) => {}
                        Err(error) => {
                            debug_adapter.send_error_response(&error)?;
                            return Err(error);
                        }
                    }
                    core_data
                }
                Err(error) => {
                    debug_adapter.send_error_response(&error)?;
                    return Err(error);
                }
            };

            if self.debugger_options.flashing_enabled && self.debugger_options.reset_after_flashing
            {
                debug_adapter
                    .restart(&mut core_data, None)
                    .context("Failed to restart core")?;
            }
        }

        // After flashing and forced setup, we can signal the client that are ready to receive incoming requests.
        // Send the `initalized` event to client.
        if debug_adapter
            .send_event::<Event>("initialized", None)
            .is_err()
        {
            let error =
                DebuggerError::Other(anyhow!("Failed sending 'initialized' event to DAP Client"));

            debug_adapter.send_error_response(&error)?;

            return Err(error);
        }

        // Loop through remaining (user generated) requests and send to the [processs_request] method until either the client or some unexpected behaviour termintates the process.
        loop {
            match self.process_next_request(&mut session_data, &mut debug_adapter) {
                Ok(DebuggerStatus::ContinueSession) => {
                    // Validate and if necessary, initialize the RTT structure.
                    if self.debugger_options.rtt.enabled
                        && session_data.rtt_connection.is_none()
                        && !(debug_adapter.last_known_status == CoreStatus::Unknown
                            || debug_adapter.last_known_status.is_halted())
                    // Do not attempt this until we have processed the MSDAP request for "configurationDone" ...
                    {
                        let target_memory_map = session_data.session.target().memory_map.clone();
                        let mut core_data =
                            match session_data.attach_core(self.debugger_options.core_index) {
                                Ok(core_data) => core_data,
                                Err(error) => {
                                    debug_adapter.send_error_response(&error)?;
                                    return Err(error);
                                }
                            };
                        log::info!("Attempting to initialize the RTT.");
                        // RTT can only be initialized if the target application has been allowed to run to the point where it does the RTT initialization.
                        // If the target halts before it processes this code, then this RTT intialization silently fails, and will try again later ...
                        // See `probe-rs-rtt::Rtt` for more information.
                        // We can safely unwrap() program_binary here, because it is validated to exist at startup of the debugger
                        #[allow(clippy::unwrap_used)]
                        core_data.attach_to_rtt(
                            &mut debug_adapter,
                            &target_memory_map,
                            self.debugger_options.program_binary.as_ref().unwrap(),
                            &self.debugger_options.rtt,
                        )?;
                    }
                }
                Ok(DebuggerStatus::TerminateSession) => {
                    return Ok(DebuggerStatus::TerminateSession);
                }
                Err(e) => {
                    debug_adapter.show_message(
                        MessageSeverity::Error,
                        format!(
                            "Debug Adapter terminated unexpectedly with an error: {:?}",
                            e
                        ),
                    );
                    debug_adapter
                        .send_event("terminated", Some(TerminatedEventBody { restart: None }))?;
                    debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }))?;
                    // Keep the process alive for a bit, so that VSCode doesn't complain about broken pipes.
                    for _loop_count in 0..10 {
                        thread::sleep(Duration::from_millis(50));
                    }
                    return Err(e);
                }
            }
        }
    }
}

pub fn list_connected_devices() -> Result<()> {
    let connected_devices = Probe::list_all();

    if !connected_devices.is_empty() {
        println!("The following devices were found:");
        connected_devices
            .iter()
            .enumerate()
            .for_each(|(num, device)| println!("[{}]: {:?}", num, device));
    } else {
        println!("No devices were found.");
    }
    Ok(())
}

pub fn list_supported_chips() -> Result<()> {
    println!("Available chips:");
    for family in
        probe_rs::config::families().map_err(|e| anyhow!("Families could not be read: {:?}", e))?
    {
        println!("{}", &family.name);
        println!("    Variants:");
        for variant in family.variants() {
            println!("        {}", variant.name);
        }
    }

    Ok(())
}

pub fn debug(debugger_options: DebuggerOptions, vscode: bool) -> Result<()> {
    let program_name = clap::crate_name!();

    let mut debugger = Debugger::new(debugger_options);

    println!(
        "{} CONSOLE: Starting as a DAP Protocol server",
        &program_name
    );
    match &debugger.debugger_options.port.clone() {
        Some(port) => {
            let addr = std::net::SocketAddr::new(
                std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                port.to_owned(),
            );

            loop {
                let listener = TcpListener::bind(addr)?;

                println!(
                    "{} CONSOLE: Listening for requests on port {}",
                    &program_name,
                    addr.port()
                );

                listener.set_nonblocking(false).ok();
                match listener.accept() {
                    Ok((socket, addr)) => {
                        socket.set_nonblocking(true).with_context(|| {
                            format!(
                                "Failed to negotiate non-blocking socket with request from :{}",
                                addr
                            )
                        })?;

                        let message =
                            format!("{}: ..Starting session from   :{}", &program_name, addr);
                        log::info!("{}", &message);
                        println!("{}", &message);
                        let reader = socket
                            .try_clone()
                            .context("Failed to establish a bi-directional Tcp connection.")?;
                        let writer = socket;

                        let dap_adapter = DapAdapter::new(reader, writer);

                        let debug_adapter = DebugAdapter::new(dap_adapter);

                        match debugger.debug_session(debug_adapter) {
                            Err(_) | Ok(DebuggerStatus::TerminateSession) => {
                                println!(
                                    "{} CONSOLE: ....Closing session from  :{}",
                                    &program_name, addr
                                );
                            }
                            Ok(DebuggerStatus::ContinueSession) => {
                                log::error!("probe-rs-debugger enountered unexpected `DebuggerStatus` in debug() execution. Please report this as a bug.");
                            }
                        }
                        // Terminate this process if it was started by VSCode
                        if vscode {
                            break;
                        }
                    }
                    Err(error) => {
                        log::error!("probe-rs-debugger failed to establish a socket connection. Reason: {:?}", error);
                    }
                }
            }
            println!("{} CONSOLE: DAP Protocol server exiting", &program_name);
        }
        None => {
            log::error!("Using probe-rs-debugger as a debug server, requires the use of the `--port` option. Please use the `--help` option for additional information");
        }
    };

    Ok(())
}
