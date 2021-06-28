use crate::debug_adapter::*;
use crate::{
    dap_types::*,
    rtt::channel::{ChannelConfig, DataFormat},
};
use crate::{debug_adapter::DapStatus, rtt::app::App};

use crate::DebuggerError;
use anyhow::{anyhow, Result};
use capstone::{arch::arm::ArchMode, prelude::*, Capstone, Endian};
use probe_rs::debug::DebugInfo;
use probe_rs::flashing::{download_file, download_file_with_options, DownloadOptions, Format};
use probe_rs::{config::TargetSelector, ProbeCreationError};
use probe_rs::{
    Core, CoreStatus, DebugProbeError, DebugProbeSelector, MemoryInterface, Probe, Session,
    WireProtocol,
};
use probe_rs_rtt::{Rtt, ScanRegion};
use serde::Deserialize;
use std::{
    env::{current_dir, set_current_dir},
    fs::{self, File},
    io,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, ToSocketAddrs},
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use structopt::StructOpt;

fn parse_protocol(src: &str) -> Result<WireProtocol, String> {
    WireProtocol::from_str(src)
}

fn default_wire_protocol() -> Option<WireProtocol> {
    Some(WireProtocol::Swd)
}

fn default_console_log() -> Option<ConsoleLog> {
    Some(ConsoleLog::Error)
}

fn parse_console_log(src: &str) -> Result<ConsoleLog, String> {
    ConsoleLog::from_str(src)
}

fn default_channel_configs() -> Vec<ChannelConfig> {
    vec![]
}

fn parse_probe_selector(src: &str) -> Result<DebugProbeSelector, String> {
    match DebugProbeSelector::from_str(src) {
        Ok(probe_selector) => Ok(probe_selector),
        Err(error) => Err(error.to_string()),
    }
}

/// The level of information to be logged to the debugger console
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ConsoleLog {
    Error,
    Info,
    Debug,
} //TODO: It would be nice instead to tap into log write once the DebugAdapter has been initialized, and intercept RUST like log info

impl std::str::FromStr for ConsoleLog {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "error" => Ok(ConsoleLog::Error),
            "info" => Ok(ConsoleLog::Info),
            "debug" => Ok(ConsoleLog::Debug),
            _ => Err(format!(
                "'{}' is not a valid console log level. Choose from [error, info, debug].",
                s
            )),
        }
    }
}

/// Shared options for all commands which use a specific probe
#[derive(StructOpt, Clone, Deserialize, Debug, Default)]
pub struct DebuggerOptions {
    /// Path to the requested working directory for the debugger
    #[structopt(long, parse(from_os_str), conflicts_with("dap"))]
    cwd: Option<PathBuf>,

    /// Binary to debug as a path. Relative to `cwd`, or fully qualified.
    #[structopt(long, parse(from_os_str), conflicts_with("dap"))]
    program_binary: Option<PathBuf>,

    /// The number associated with the debug probe to use. Use 'list' command to see available probes
    #[structopt(
        long = "probe",
        parse(try_from_str = parse_probe_selector),
        help = "Use this flag to select a specific probe in the list.\n\
                    Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID."
    )]
    #[serde(alias = "probe")]
    pub(crate) probe_selector: Option<DebugProbeSelector>,

    /// The MCU Core to debug. Default is 0
    #[structopt(long = "core-index", default_value)]
    #[serde(default)]
    pub(crate) core_index: usize,

    /// The target to be selected.
    #[structopt(short, long, conflicts_with("dap"))]
    pub(crate) chip: Option<String>,

    /// Protocol to use for target connection
    #[structopt(short, long, parse(try_from_str = parse_protocol))]
    #[serde(default = "default_wire_protocol")]
    pub(crate) protocol: Option<WireProtocol>,

    /// Protocol speed in kHz
    #[structopt(short, long, conflicts_with("dap"))]
    pub(crate) speed: Option<u32>,

    /// Assert target's reset during connect
    #[structopt(long, conflicts_with("dap"))]
    #[serde(default)]
    pub(crate) connect_under_reset: bool,

    /// IP port number to listen for incoming DAP connections, e.g. "50000"
    #[structopt(long, requires("dap"))]
    pub(crate) port: Option<u16>,

    /// Flash the target before debugging
    #[structopt(long)]
    #[serde(default)]
    pub(crate) flashing_enabled: bool,

    /// Reset the target after flashing
    #[structopt(long, hidden = true, required_if("flashing_enabled", "true"))]
    #[serde(default)]
    pub(crate) reset_after_flashing: bool,

    /// Halt the target after reset
    #[structopt(long, hidden = true)]
    #[serde(default)]
    pub(crate) halt_after_reset: bool,

    /// Do a full chip erase, versus page-by-page erase
    #[structopt(long, hidden = true, required_if("flashing_enabled", "true"))]
    #[serde(default)]
    pub(crate) full_chip_erase: bool,

    /// Restore erased bytes that will not be rewritten from ELF
    #[structopt(long, hidden = true, required_if("flashing_enabled", "true"))]
    #[serde(default)]
    pub(crate) restore_unwritten_bytes: bool,

    /// Level of information to be logged to the debugger console (Error, Info or Debug )
    #[structopt(long, parse(try_from_str = parse_console_log))]
    #[serde(default = "default_console_log")]
    pub(crate) console_log_level: Option<ConsoleLog>,

    #[structopt(flatten)]
    #[serde(flatten)]
    pub(crate) rtt: RttConfig,
}

impl DebuggerOptions {
    /// Validate the new cwd, or else set it from the environment.
    pub(crate) fn validate_and_update_cwd(&mut self, new_cwd: Option<PathBuf>) {
        self.cwd = match new_cwd {
            Some(temp_path) => {
                if temp_path.is_dir() {
                    Some(temp_path)
                } else {
                    Some(current_dir().expect("Cannot use current working directory. Please check existence and permissions."))
                }
            }
            None => Some(current_dir().expect(
                "Cannot use current working directory. Please check existence and permissions.",
            )),
        };
    }

    /// If the path to the programm to be debugged is relative, we join if with the cwd.
    pub(crate) fn qualify_and_update_program_binary(
        &mut self,
        new_program_binary: Option<PathBuf>,
    ) {
        self.program_binary = match new_program_binary {
            Some(temp_path) => {
                let mut new_path = PathBuf::new();
                if temp_path.is_relative() {
                    new_path.push(self.cwd.clone().unwrap());
                }
                new_path.push(temp_path);
                Some(new_path)
            }
            None => None,
        };
    }
}

//TODO: Implement an option to detect channels and use them as defaults. To simplify the case where developers want to get started with all the RTT channels configured in their app.
#[derive(StructOpt, Debug, Clone, Deserialize, Default)]
pub struct RttConfig {
    #[structopt(skip)]
    #[serde(rename = "rtt_enabled")]
    pub enabled: bool,
    #[structopt(skip)]
    #[serde(default = "default_channel_configs", rename = "rtt_channels")]
    pub channels: Vec<ChannelConfig>,
    /// Connection timeout in ms.
    #[structopt(skip)]
    #[serde(rename = "rtt_timeout")]
    pub timeout: usize,
    /// Whether to show timestamps in RTTUI
    #[structopt(skip)]
    #[serde(rename = "rtt_show_timestamps")]
    pub show_timestamps: bool,
}

/// #Debugger Overview
/// The Debugger struct and it's implementation supports both CLI and DAP requests. On startup, the command line arguments are checked for validity, then executed by a dedicated method, and results/errors are wrapped for appropriate CLI or DAP handling
/// ## Usage: CLI for `probe-rs`
/// The CLI accepts commands on STDIN and returns results to STDOUT, while all LOG actions are sent to STDERR.
/// - `probe-rs-debug --help` to list available commands and flags. All of the commands, except for `debug` will execute and then return to the OS.
/// - `probe-rs-debug debug --help` to list required and optional options for debug mode. The `debug` command will accept and process incoming requests until the user, or a fatal error, ends the session.
/// ## Usage: DAP Server for `probe-rs`
/// The DAP Server can run in one of two modes.
/// - `probe-rs-debug --debug --dap <other options>` : Uses STDIN and STDOUT to service DAP requests. For example, a VSCode `Launch` request prefers this mode.
/// - `probe-rs-debug --debug --dap --port <IP port number> <other options>` : Uses TCP Sockets to the defined IP port number to service DAP requests. For example, a VSCode `Attach` request prefers this mode.
pub struct Debugger {
    debugger_options: DebuggerOptions,
    all_commands: Vec<DebugCommand>,
    pub supported_commands: Vec<DebugCommand>,
    /// The optional RTT instance
    rtt_app: Option<App>,
}

pub struct SessionData {
    pub(crate) session: Arc<Mutex<Session>>,
    #[allow(dead_code)]
    pub(crate) capstone: Capstone,
}

pub struct CoreData<'p> {
    pub(crate) target_core: Core<'p>,
    pub(crate) target_name: String,
    pub(crate) debug_info: Option<DebugInfo>,
}

/// Definition of commands that have been implemented in Debugger.
#[derive(Clone, Copy)]
pub struct DebugCommand {
    /// Has value if it can be called from DAP, else ""
    pub(crate) dap_cmd: &'static str,
    /// Has value if it can be called from CLI, else ""
    pub(crate) cli_cmd: &'static str,
    /// Help message to be displayed if invalid usage is attempted
    pub(crate) help_text: &'static str,
    /// The function that will be called when this command is intiated. It returns data via the DebugAdapter send_response() methods, so the only return from the function is and hint to the caller on whether it should continue with other commands, or terminate.
    pub(crate) function_name: &'static str,
    //TODO: Need to be able to pass DebugAdapter<R,W> as a parameter then we can simplify the DebugAdapter::process_next_request() match statement to invoke the function from a pointer.
    //pub(crate) function: fn(core_data: &mut CoreData, request: &Request) -> bool,
}
pub fn start_session(debugger_options: &DebuggerOptions) -> Result<SessionData, DebuggerError> {
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
            // Only automatically select a probe if there is only
            // a single probe detected.
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
    //set the protocol
    target_probe.select_protocol(debugger_options.protocol.unwrap_or(WireProtocol::Swd))?;

    //set the speed
    if let Some(speed) = debugger_options.speed {
        let actual_speed = target_probe.set_speed(speed).unwrap();
        if actual_speed != speed {
            log::warn!(
                "Protocol speed {} kHz not supported, actual speed is {} kHz",
                speed,
                actual_speed
            );
        }
    }
    //attach a Session to the probe
    let target_session = if debugger_options.connect_under_reset {
        target_probe.attach_under_reset(target_selector)?
    } else {
        target_probe.attach(target_selector).map_err(|err| {
            anyhow!(
                "Error attaching to the probe: {:?}.\nTry the --connect-under-reset option",
                err
            )
        })?
    };
    //Change the current working directory if a debugger_option exists
    if let Some(new_cwd) = debugger_options.cwd.clone() {
        set_current_dir(new_cwd.as_path()).map_err(|err| {
            anyhow!(
                "Failed to set current working directory to: {:?}, {:?}",
                new_cwd,
                err
            )
        })?;
    };

    //Configure the Capstone
    let capstone = Capstone::new()
        .arm()
        .mode(ArchMode::Thumb)
        .endian(Endian::Little)
        .build()
        .unwrap();

    //Populate the return SessionData
    Ok(SessionData {
        session: Arc::new(Mutex::new(target_session)),
        capstone,
    })
}

pub fn attach_core<'p>(
    session: &'p mut Session,
    debugger_options: &DebuggerOptions,
) -> Result<CoreData<'p>, DebuggerError> {
    //Configure the DebugInfo
    let debug_info = debugger_options
        .program_binary
        .as_ref()
        .and_then(|path| DebugInfo::from_file(path).ok());
    // TODO: Change this expect laer on maybe.
    let target_name = session.target().name.clone();
    //Do no-op attach to the core and return it
    match session.core(debugger_options.core_index) {
        Ok(target_core) => Ok(CoreData {
            target_core,
            target_name: format!("{}-{}", debugger_options.core_index, target_name),
            debug_info,
        }),
        Err(_) => Err(DebuggerError::UnableToOpenProbe(Some(
            "No core at the specified index.",
        ))),
    }
}

impl Debugger {
    pub fn new(debugger_options: DebuggerOptions) -> Self {
        //Define all the commands supported by the debugger
        //TODO: There is a lot of repetitive code here, and a great opportunity for macros
        //TODO: Implement command completion and saved-history for rustyline CLI
        //TODO: Implement DAP Evaluate to allow CLI commands to be processed though the VSCode Debug Console REPLE

        Self {
            debugger_options,
            all_commands: vec![
                DebugCommand {
                    dap_cmd: "",
                    cli_cmd: "status",
                    help_text: "Show current status of CPU",
                    function_name: "status",
                },
                DebugCommand {
                    dap_cmd: "next",
                    cli_cmd: "step",
                    help_text: "Step a single instruction",
                    function_name: "next",
                },
                DebugCommand {
                    dap_cmd: "pause",
                    cli_cmd: "halt",
                    help_text: "Stop the CPU",
                    function_name: "pause",
                },
                DebugCommand {
                    dap_cmd: "read_memory",
                    cli_cmd: "read",
                    help_text: "Read 32bit value from memory",
                    function_name: "read_memory",
                },
                DebugCommand {
                    dap_cmd: "",
                    cli_cmd: "write",
                    help_text: "Write a 32bit value to memory",
                    function_name: "write",
                },
                DebugCommand {
                    dap_cmd: "",
                    cli_cmd: "set_breakpoint",
                    help_text: "Set a breakpoint at a specifc address",
                    function_name: "set_breakpoint",
                },
                DebugCommand {
                    dap_cmd: "",
                    cli_cmd: "clear_breakpoint",
                    help_text: "Clear a breakpoint",
                    function_name: "clear_breakpoint",
                },
                //TODO: These need to be implemented in debug_adapter.rs if we're going to keep them
                // DebugCommand {
                //     dap_cmd: "",
                //     cli_cmd: "show_cpu_register_values",
                //     help_text: "Show CPU register values",
                //     function_name: "show_cpu_register_values",
                // },
                // DebugCommand {
                //     dap_cmd: "",
                //     cli_cmd: "dump_cpu_state",
                //     help_text: "Store a dump of the current CPU state",
                //     function_name: "dump_cpu_state",
                // },
                DebugCommand {
                    dap_cmd: "",
                    cli_cmd: "reset",
                    help_text: "Reset the device attached to the debug probe",
                    function_name: "restart",
                },
                DebugCommand {
                    dap_cmd: "configurationDone",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "configuration_done",
                },
                DebugCommand {
                    dap_cmd: "disconnect",
                    cli_cmd: "quit",
                    help_text: "Disconnect from the probe and end the debug session",
                    function_name: "disconnect",
                },
                DebugCommand {
                    dap_cmd: "terminate",
                    cli_cmd: "",
                    help_text: "Disconnect from the probe and end the debug session",
                    function_name: "terminate",
                },
                DebugCommand {
                    dap_cmd: "threads",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "threads",
                },
                DebugCommand {
                    dap_cmd: "setBreakpoints",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "set_breakpoints",
                },
                DebugCommand {
                    dap_cmd: "stackTrace",
                    cli_cmd: "stack",
                    help_text: "Show stack trace (back trace)",
                    function_name: "stack_trace",
                },
                DebugCommand {
                    dap_cmd: "scopes",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "scopes",
                },
                DebugCommand {
                    dap_cmd: "source",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "source",
                },
                DebugCommand {
                    dap_cmd: "variables",
                    cli_cmd: "",
                    help_text: "",
                    function_name: "variables",
                },
                DebugCommand {
                    dap_cmd: "run",
                    cli_cmd: "continue",
                    help_text: "Resume execution of target",
                    function_name: "continue",
                },
            ],
            supported_commands: vec![],
            rtt_app: None,
        }
    }

    //SECTION: Methods to handle DAP requests
    pub fn process_next_request<R: Read, W: Write>(
        &mut self,
        session_data: &mut SessionData,
        debug_adapter: &mut DebugAdapter<R, W>,
    ) -> bool {
        let request = debug_adapter.listen_for_request();
        match request.command.as_ref() {
            "process_next_request" => {
                /*
                The logic of this command is as follows:
                - While we are waiting for DAP-Client (TCP or STDIO), we have to continuously check in on the status of the probe.
                - Initally, while `LAST_KNOWN_STATUS` probe-rs::CoreStatus::Unknown, we do nothing. Wait until latter part of `debug_session` sets it to something known.
                - If the `LAST_KNOWN_STATUS` is `Halted`, then we stop polling the Probe until the next DAP-Client request attempts an action
                - If the `new_status` is an Err, then the probe is no longer available, and we  end the debugging session
                - If the `new_status` is different from the `LAST_KNOWN_STATUS`, then we have to tell the DAP-Client by way of an `Event`
                - If the `new_status` is `Running`, then we have to poll on a regular basis, until the Probe stops for good reasons like breakpoints, or bad reasons like panics. Then tell the DAP-Client.
                - TODO: Figure out CPU/Comms overhead costs to determine optimal polling intervals
                */
                let last_known_status = debug_adapter.last_known_status;
                match last_known_status {
                    CoreStatus::Unknown => true,
                    _other => {
                        // Use every opportunity to poll the RTT channels for data
                        let mut received_rtt_data = false;
                        if let Some(ref mut rtt_app) = self.rtt_app {
                            let data_packet = rtt_app.poll_rtt();
                            if data_packet.len() > 0 {
                                received_rtt_data = true;
                                for (rtt_channel, rtt_data) in data_packet {
                                    debug_adapter.rtt_output(
                                        rtt_channel.parse::<usize>().unwrap_or(0),
                                        rtt_data,
                                    );
                                }
                            }
                        }

                        //Check and update the core status.
                        let mut session = session_data
                            .session
                            .lock()
                            .expect("The other thread accessing the session crashed.");
                        let mut core_data = match attach_core(&mut session, &self.debugger_options)
                        {
                            Ok(core_data) => core_data,
                            Err(error) => {
                                debug_adapter.send_response::<()>(&request, Err(error));
                                return false;
                            }
                        };
                        let new_status = match core_data.target_core.status() {
                            Ok(new_status) => new_status,
                            Err(error) => {
                                debug_adapter.send_response::<()>(
                                    &request,
                                    Err(DebuggerError::ProbeRs(error)),
                                );
                                return false;
                            }
                        };

                        // Only sleep (nap for a short duration) IF the probe's status hasn't changed AND there was no RTT data in the last poll. Otherwise loop again to keep things flowing as fast as possible. The justification is that any client side CPU used to keep polling is a small price to pay for maximum throughput of debug requests and RTT from the probe.
                        if new_status == last_known_status && !received_rtt_data {
                            thread::sleep(Duration::from_millis(50)); //small delay to reduce fast looping costs
                            return true;
                        };

                        // TODO: Remove ... println!("process_next_request: last_known_status={:?}\tnew_status={:?}\treceived_rtt_data{:?}", last_known_status, new_status, received_rtt_data);
                        match new_status {
                            CoreStatus::Running | CoreStatus::Sleeping => {
                                let event_body = Some(ContinuedEventBody {
                                    all_threads_continued: Some(true),
                                    thread_id: core_data.target_core.id() as i64,
                                });
                                debug_adapter.send_event("continued", event_body);
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
                                debug_adapter.send_event("stopped", event_body);
                            }
                            CoreStatus::LockedUp => {
                                let event_body = Some(StoppedEventBody {
                                    reason: new_status.short_long_status().0.to_owned(),
                                    description: Some(new_status.short_long_status().1.to_owned()),
                                    thread_id: Some(core_data.target_core.id() as i64),
                                    preserve_focus_hint: Some(false),
                                    text: None,
                                    all_threads_stopped: Some(true),
                                    hit_breakpoint_ids: None,
                                });
                                debug_adapter.send_event("stopped", event_body);
                                return false;
                            }
                            CoreStatus::Unknown => {
                                debug_adapter.send_response::<()>(
                                    &request,
                                    Err(DebuggerError::Other(anyhow!(
                                        "Unknown Device status reveived from Probe-rs"
                                    ))),
                                );
                                return false;
                            }
                        };
                        debug_adapter.last_known_status = new_status;
                        true
                    }
                }
            }
            "error" | "quit" => {
                // The listen_for_request would have reported this, so we just have to exit.
                false
            }
            "help" => {
                println!("The following commands are available:");
                for cmd in self.supported_commands.iter() {
                    println!(" - {:<30} : {}", cmd.cli_cmd, cmd.help_text);
                }
                true
            }
            command_lookup => {
                let valid_command = self
                    .supported_commands
                    .iter()
                    .find(|c| c.dap_cmd == command_lookup || c.cli_cmd == command_lookup);
                match valid_command {
                    Some(valid_command) => {
                        // First, attach to the core.
                        let mut session = session_data
                            .session
                            .lock()
                            .expect("The other thread accessing the session crashed.");
                        let mut core_data = match attach_core(&mut session, &self.debugger_options)
                        {
                            Ok(core_data) => core_data,
                            Err(error) => {
                                debug_adapter.send_response::<()>(&request, Err(error));
                                return false;
                            }
                        };
                        match valid_command.function_name {
                            "status" => debug_adapter.status(&mut core_data, &request),
                            "next" => debug_adapter.next(&mut core_data, &request),
                            "pause" => debug_adapter.pause(&mut core_data, &request),
                            "read_memory" => debug_adapter.read_memory(&mut core_data, &request),
                            "write" => debug_adapter.write(&mut core_data, &request),
                            "set_breakpoint" => {
                                debug_adapter.set_breakpoint(&mut core_data, &request)
                            }
                            "clear_breakpoint" => {
                                debug_adapter.clear_breakpoint(&mut core_data, &request)
                            }
                            "show_cpu_register_values" => {
                                debug_adapter.show_cpu_register_values(&mut core_data, &request)
                            }
                            "dump_cpu_state" => {
                                debug_adapter.dump_cpu_state(&mut core_data, &request)
                            }
                            "configuration_done" => {
                                debug_adapter.configuration_done(&mut core_data, &request)
                            }
                            "disconnect" => debug_adapter.disconnect(&mut core_data, &request),
                            "terminate" => debug_adapter.terminate(&mut core_data, &request),
                            "threads" => debug_adapter.threads(&mut core_data, &request),
                            "restart" => debug_adapter.restart(&mut core_data, &request),
                            "set_breakpoints" => {
                                debug_adapter.set_breakpoints(&mut core_data, &request)
                            }
                            "stack_trace" => debug_adapter.stack_trace(&mut core_data, &request),
                            "scopes" => debug_adapter.scopes(&mut core_data, &request),
                            "source" => debug_adapter.source(&mut core_data, &request),
                            "variables" => debug_adapter.variables(&mut core_data, &request),
                            "continue" => debug_adapter.r#continue(&mut core_data, &request),
                            other => {
                                debug_adapter.send_response::<()>(
                                    &request,
                                    Err(DebuggerError::Other(anyhow!("Received request '{}', which is not supported or not implemented yet", other))),
                                );
                                true
                            }
                        }
                    }
                    None => {
                        //Unimplemented command
                        if debug_adapter.adapter_type == DebugAdapterType::DapClient {
                            debug_adapter.log_to_console(format!(
                                "Received unsupported request '{}'\n",
                                command_lookup
                            ));
                            debug_adapter
                                    .send_response::<()>(
                                        &request,
                                        Err(DebuggerError::Other(anyhow!(
                                        "Received request '{}', which is not supported or not implemented yet",
                                        command_lookup
                                    )
                                        )),
                                    );
                            true
                        } else {
                            debug_adapter.send_response::<()>(
                                &request,
                                Err(DebuggerError::Other(anyhow!(
                                    "Unknown command '{}'. Enter 'help' for a list of commands",
                                    command_lookup
                                ))),
                            );
                            true
                        }
                    }
                }
            }
        }
    }

    /** debug_session(..) is where the primary _debug processing_ for the DAP (Debug Adapter Protocol) adapter happens.
    All requests are interpreted, actions taken, and responses formulated here. This function is self contained and returns nothing.
    The [debug_adapter::DebugAdapter] takes care of _implementing the DAP Base Protocol_ and _communicating with the DAP client_ and _probe_.
    */
    pub fn debug_session<R: Read, W: Write>(&mut self, mut debug_adapter: DebugAdapter<R, W>) {
        //Filter out just the set of commands that will work for this session.
        self.supported_commands = if debug_adapter.adapter_type == DebugAdapterType::DapClient {
            self.all_commands
                .iter()
                .filter(|x| !x.dap_cmd.is_empty())
                .cloned()
                .collect()
        } else {
            self.all_commands
                .iter()
                .filter(|x| !x.cli_cmd.is_empty())
                .cloned()
                .collect()
        };

        //Create a custom request to use in responses for errors, etc. where no specific incoming request applies.
        let custom_request = Request {
            arguments: None,
            command: "probe_rs_setup_during_initialize".to_owned(),
            seq: debug_adapter.peek_seq(),
            type_: "request".to_owned(),
        };

        //The DapClient startup process has a specific sequence. Handle it here before starting a probe-rs session and looping through user generated requests.
        if debug_adapter.adapter_type == DebugAdapterType::DapClient {
            //Handling the initialize, and Attach/Launch requests here in this method, before entering the interactive loop that processes requests through the process_request method.

            //Initialize request
            #[allow(unused_assignments)]
            let mut request = Request {
                arguments: None,
                command: "error".to_string(),
                seq: 0,
                type_: "request".to_owned(),
            };
            loop {
                request = debug_adapter.listen_for_request();
                match request.command.as_str() {
                    "process_next_request" => continue,
                    "error" => return,
                    "initialize" => break, //We have lift- off
                    other => {
                        debug_adapter.send_response::<()>(
                            &request,
                            Err(
                                anyhow!("Initial command was '{}', expected 'initialize'", other)
                                    .into(),
                            ),
                        );
                        return;
                    }
                };
            }
            let _arguments: InitializeRequestArguments = match get_arguments::<
                InitializeRequestArguments,
            >(&request)
            {
                Ok(arguments) => {
                    if arguments.columns_start_at_1.unwrap() && arguments.lines_start_at_1.unwrap()
                    {
                    } else {
                        debug_adapter.send_response::<()>(&request, Err(DebuggerError::Other(anyhow!("Unsupported Capability: Client requested column and row numbers start at 0."))));
                        return;
                    }
                    arguments
                }
                Err(error) => {
                    debug_adapter.send_response::<()>(&request, Err(error));
                    return;
                }
            };

            //Reply to Initialize with Capabilities
            let capabilities = Capabilities {
                supports_configuration_done_request: Some(true),
                supports_read_memory_request: Some(true),
                supports_restart_request: Some(false), // It is better (and cheap enough) to let the client kill and restart the debugadapter, than to try a in-process reset.
                supports_terminate_request: Some(true),
                // supports_value_formatting_options: Some(true),
                //supports_function_breakpoints: Some(true),
                //TODO: Use DEMCR register to implement exception breakpoints
                // supports_exception_options: Some(true),
                // supports_exception_filter_options: Some (true),
                ..Default::default()
            };
            debug_adapter.send_response(&request, Ok(Some(capabilities)));

            //Process either the Launch or Attach request
            request.command = "error".to_owned();
            loop {
                request = debug_adapter.listen_for_request();
                match request.command.as_str() {
                    "process_next_request" => continue,
                    "error" => return,
                    "attach" | "launch" => break, //OK
                    other => {
                        debug_adapter.send_response::<()>(
                            &request,
                            Err(DebuggerError::Other(anyhow!(
                                "Expected request 'launch' or 'attach', but received' {}'",
                                other
                            ))),
                        );
                        return;
                    }
                };
            }
            match get_arguments(&request) {
                Ok(arguments) => {
                    self.debugger_options = DebuggerOptions { ..arguments };
                    debug_adapter.console_log_level = self
                        .debugger_options
                        .console_log_level
                        .unwrap_or(ConsoleLog::Error);
                    //update the cwd and program_binary
                    self.debugger_options
                        .validate_and_update_cwd(self.debugger_options.cwd.clone());
                    self.debugger_options.qualify_and_update_program_binary(
                        self.debugger_options.program_binary.clone(),
                    );
                    match self.debugger_options.program_binary.clone() {
                        Some(program_binary) => {
                            if !program_binary.is_file() {
                                debug_adapter.send_response::<()>(
                                    &request,
                                    Err(DebuggerError::Other(anyhow!(
                                        "Invalid program binary file specified '{:?}'",
                                        program_binary
                                    ))),
                                );
                                return;
                            }
                        }
                        None => {
                            debug_adapter.send_response::<()>(
                                &request,
                                Err(DebuggerError::Other(anyhow!(
                                "Please use the --program-binary option to specify an executable"
                            ))),
                            );
                            return;
                        }
                    }
                    debug_adapter.send_response::<()>(&request, Ok(None));
                }
                Err(error) => {
                    debug_adapter.send_response::<()>(
                        &request,
                        Err(DebuggerError::Other(anyhow!(
                        "Could not derive DebuggerOptions from request '{}', with arguments {:?}\n{:?} ", request.command, request.arguments, error
                    ))));
                    return;
                }
            };
        } else {
            //DebugAdapterType::CommandLine
            //update the cwd and program_binary
            self.debugger_options
                .validate_and_update_cwd(self.debugger_options.cwd.clone());
            self.debugger_options
                .qualify_and_update_program_binary(self.debugger_options.program_binary.clone());
            match self.debugger_options.program_binary.clone() {
                Some(program_binary) => {
                    if !program_binary.is_file() {
                        debug_adapter.send_response::<()>(
                            &custom_request,
                            Err(DebuggerError::Other(anyhow!(
                                "Invalid program binary file specified '{:?}'",
                                program_binary
                            ))),
                        );
                        return;
                    }
                }
                None => {
                    debug_adapter.send_response::<()>(
                        &custom_request,
                        Err(DebuggerError::Other(anyhow!(
                            "Please use the --program-binary option to specify an executable"
                        ))),
                    );
                    return;
                }
            }
        }

        let mut session_data = match start_session(&self.debugger_options) {
            Ok(session_data) => session_data,
            Err(error) => {
                debug_adapter.send_response::<()>(
                    &Request {
                        arguments: None,
                        command: "probe-rs::openProbe".to_owned(),
                        seq: debug_adapter.peek_seq(),
                        type_: "request".to_owned(),
                    },
                    Err(error),
                );
                debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }));
                return;
            }
        };
        debug_adapter.halt_after_reset = self.debugger_options.halt_after_reset;

        //Do the flashing
        {
            if self.debugger_options.flashing_enabled {
                let path_to_elf = self.debugger_options.program_binary.clone().unwrap();
                debug_adapter.log_to_console(format!(
                    "FLASHING: Starting write of {:?} to device memory",
                    &path_to_elf
                ));

                let mut download_options = DownloadOptions::default();

                download_options.keep_unwritten_bytes =
                    self.debugger_options.restore_unwritten_bytes;

                download_options.do_chip_erase = self.debugger_options.full_chip_erase;

                match download_file_with_options(
                    &mut session_data
                        .session
                        .lock()
                        .expect("The other thread accessing the session crashed."),
                    path_to_elf,
                    Format::Elf,
                    download_options,
                ) {
                    Ok(_) => {
                        debug_adapter.log_to_console(format!(
                            "FLASHING: Completed write of {:?} to device memory",
                            &self.debugger_options.program_binary.clone().unwrap()
                        ));
                    }
                    Err(error) => {
                        debug_adapter.send_response::<()>(
                            &custom_request,
                            Err(DebuggerError::FileDownload(error)),
                        );
                        return;
                    }
                }
            }
        }

        self.rtt_app = if self.debugger_options.rtt.enabled {
            // Attach to RTT on the probe
            attach_to_rtt(session_data.session.clone(), &self.debugger_options).ok()
        } else {
            debug_adapter.log_to_console("No RTT configured.");
            None
        };

        // This is the first attach to the requested core. If this one works, all subsequent ones will be no-op requests for a Core reference. Do NOT hold onto this reference for the duration of the session ... that is why this code is in a block of its own.
        {
            // First, attach to the core
            let mut session = session_data
                .session
                .lock()
                .expect("The other thread accessing the session crashed.");
            let mut core_data = match attach_core(&mut session, &self.debugger_options) {
                Ok(core_data) => core_data,
                Err(error) => {
                    debug_adapter.send_response::<()>(&custom_request, Err(error));
                    return;
                }
            };

            if self.debugger_options.flashing_enabled
                && self.debugger_options.reset_after_flashing
                && !debug_adapter.restart(&mut core_data, &custom_request)
            {
                return;
            }
        }

        //After flashing and forced setup, we can signal the client that are ready to receive incoming requests
        //Send Initalized event to client
        if !debug_adapter.send_event::<Event>("initialized", None) {
            debug_adapter.send_response::<()>(
                &custom_request,
                Err(DebuggerError::Other(anyhow!(
                    "Failed sending 'initialized' event to DAP Client"
                ))),
            );
            debug_adapter.send_event("exited", Some(ExitedEventBody { exit_code: 1 }));
            return;
        }
        //Loop through remaining (user generated) requests and send to the [processs_request] method until either the client or some unexpected behaviour termintates the process.
        loop {
            if !self.process_next_request(&mut session_data, &mut debug_adapter) {
                //DapClient STEP FINAL: Let the client know that we are done and exiting
                if debug_adapter.adapter_type == DebugAdapterType::DapClient {
                    debug_adapter
                        .send_event("terminated", Some(TerminatedEventBody { restart: None }));
                }
                break;
            }
        }
        //Exiting this function means we the debug_session is complete and we are done. End of process.
        //TODO: Add functionality to keep the server alive, respond to DAP Client sessions that end, and accept new session requests.
    }
}
// SECTION: Functions for CLI struct matches from main.rs

pub fn attach_to_rtt(
    session: Arc<Mutex<Session>>,
    debugger_options: &DebuggerOptions,
) -> Result<crate::rtt::app::App, anyhow::Error> {
    let defmt_enable = debugger_options
        .rtt
        .channels
        .iter()
        .any(|elem| elem.format == DataFormat::Defmt);
    let defmt_state = if defmt_enable {
        // TODO: Clean the unwraps.
        let elf = fs::read(debugger_options.program_binary.clone().unwrap()).unwrap();
        let table = defmt_decoder::Table::parse(&elf)?;

        let locs = {
            let table = table.as_ref().unwrap();
            let locs = table.get_locations(&elf)?;

            if !table.is_empty() && locs.is_empty() {
                log::warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
                None
            } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
                Some(locs)
            } else {
                log::warn!("Location info is incomplete; it will be omitted from the output.");
                None
            }
        };
        Some((table.unwrap(), locs))
    } else {
        None
    };

    let t = std::time::Instant::now();
    let mut error = None;

    let mut i = 1;

    while (t.elapsed().as_millis() as usize) < debugger_options.rtt.timeout {
        log::info!("Initializing RTT (attempt {})...", i);
        i += 1;

        let rtt_header_address = if let Ok(mut file) =
            File::open(debugger_options.program_binary.clone().unwrap().as_path())
        {
            if let Some(address) = crate::rtt::app::App::get_rtt_symbol(&mut file) {
                ScanRegion::Exact(address as u32)
            } else {
                ScanRegion::Ram
            }
        } else {
            ScanRegion::Ram
        };

        match Rtt::attach_region(session.clone(), &rtt_header_address) {
            Ok(rtt) => {
                log::info!("RTT initialized.");
                let app = crate::rtt::app::App::new(rtt, &debugger_options.rtt)?;
                return Ok(app);
            }
            Err(err) => {
                error = Some(anyhow!("Error attaching to RTT: {}", err));
            }
        };

        log::debug!("Failed to initialize RTT. Retrying until timeout.");
    }
    if let Some(error) = error {
        return Err(error);
    }
    Err(anyhow!("Rtt initialization failed."))
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

// TODO: Implement assert functionality for true, false & unspecified
pub fn reset_target_of_device(
    debugger_options: DebuggerOptions,
    _assert: Option<bool>,
) -> Result<()> {
    let session_data = start_session(&debugger_options)?;
    let mut session = session_data
        .session
        .lock()
        .expect("The other thread accessing the session crashed.");
    attach_core(&mut session, &debugger_options)
        .unwrap()
        .target_core
        .reset()?;
    Ok(())
}

pub fn dump_memory(debugger_options: DebuggerOptions, loc: u32, words: u32) -> Result<()> {
    let session_data = start_session(&debugger_options)?;
    let mut session = session_data
        .session
        .lock()
        .expect("The other thread accessing the session crashed.");
    let mut target_core = attach_core(&mut session, &debugger_options)
        .unwrap()
        .target_core;

    let mut data = vec![0_u32; words as usize];

    // Start timer.
    let instant = Instant::now();

    // let loc = 220 * 1024;

    target_core.read_32(loc, &mut data.as_mut_slice())?;
    // Stop timer.
    let elapsed = instant.elapsed();

    // Print read values.
    for word in 0..words {
        println!(
            "Addr 0x{:08x?}: 0x{:08x}",
            loc + 4 * word,
            data[word as usize]
        );
    }
    // Print stats.
    println!("Read {:?} words in {:?}", words, elapsed);
    Ok(())
}

pub fn download_program_fast(debugger_options: DebuggerOptions, path: &str) -> Result<()> {
    let session_data = start_session(&debugger_options)?;
    let mut session = session_data
        .session
        .lock()
        .expect("The other thread accessing the session crashed.");

    download_file(&mut session, &path, Format::Elf)?;
    Ok(())
}

pub fn trace_u32_on_target(debugger_options: DebuggerOptions, loc: u32) -> Result<()> {
    use scroll::{Pwrite, LE};
    use std::io::prelude::*;
    use std::thread::sleep;

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    let session_data = start_session(&debugger_options)?;
    let mut session = session_data
        .session
        .lock()
        .expect("The other thread accessing the session crashed.");
    let mut target_core = attach_core(&mut session, &debugger_options)
        .unwrap()
        .target_core;

    loop {
        // Prepare read.
        let elapsed = start.elapsed();
        let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

        // Read data.
        let value: u32 = target_core.read_word_32(loc)?;

        xs.push(instant);
        ys.push(value);

        // Send value to plot.py.
        let mut buf = [0_u8; 8];
        // Unwrap is safe!
        buf.pwrite_with(instant, 0, LE).unwrap();
        buf.pwrite_with(value, 4, LE).unwrap();
        std::io::stdout().write_all(&buf)?;

        std::io::stdout().flush()?;

        // Schedule next read.
        let elapsed = start.elapsed();
        let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());
        let poll_every_ms = 50;
        let time_to_wait = poll_every_ms - instant % poll_every_ms;
        sleep(Duration::from_millis(time_to_wait));
    }
}

pub fn debug(debugger_options: DebuggerOptions, dap: bool) {
    let program_name = structopt::clap::crate_name!();

    let mut debugger = Debugger::new(debugger_options);

    if !dap {
        println!(
            "Welcome to {:?}. Use the 'help' command for more",
            &program_name
        );
        let adapter = DebugAdapter::new(io::stdin(), io::stdout(), DebugAdapterType::CommandLine);
        debugger.debug_session(adapter);
    } else {
        //TODO: Implement the case where the server needs to keep running after the client has disconnected.
        println!("Starting {:?} as a DAP Protocol server", &program_name);
        match &debugger.debugger_options.port.clone() {
            Some(port) => {
                let addr = format!("{}:{:?}", Ipv4Addr::LOCALHOST.to_string(), port)
                    .to_socket_addrs()
                    .unwrap()
                    .next()
                    .unwrap(); //TODO: Implement multi-core and multi-session

                let listener = match TcpListener::bind(addr) {
                    Ok(listener) => listener,
                    Err(error) => {
                        println!("{:?}", error);
                        return;
                    }
                };

                println!("Listening for requests on :{}", addr);

                let (socket, addr) = listener.accept().unwrap();
                match socket.set_nonblocking(true) {
                    Ok(_) => {
                        println!("..Starting session from   :{}", addr);
                    }
                    Err(_) => {
                        println!(
                            "ERROR: Failed to negotiate non-blocking socket with request from :{}",
                            addr
                        );
                    }
                }

                let reader = socket.try_clone().unwrap();
                let writer = socket;

                let adapter = DebugAdapter::new(reader, writer, DebugAdapterType::DapClient);
                //TODO: When running in server mode, we want to stay open for new sessions. Implement intelligent restart in debug_session.
                debugger.debug_session(adapter);
                println!("....Closing session from  :{}", addr);
            }
            None => {
                println!(
                    "Debugger started in directory {}",
                    &current_dir().unwrap().display()
                );
                let adapter =
                    DebugAdapter::new(io::stdin(), io::stdout(), DebugAdapterType::DapClient);
                debugger.debug_session(adapter);
            }
        };
    }
}
