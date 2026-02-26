use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use probe_rs::probe::list::Lister;
use rustyline_async::SharedWriter;
use rustyline_async::{Readline, ReadlineEvent};
use time::UtcOffset;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::cmd::dap_server::debug_adapter::dap::adapter::DebugAdapter;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::EvaluateResponseBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::InitializeRequestArguments;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::OutputEventBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::ProgressStartEventBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::ProgressUpdateEventBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::{
    DisconnectArguments, EvaluateArguments, RttChannelEventBody, RttDataEventBody,
    RttWindowOpenedArguments,
};
use crate::cmd::dap_server::debug_adapter::dap::dap_types::{
    ErrorResponseBody, ShowMessageEventBody,
};
use crate::cmd::dap_server::debug_adapter::protocol::ProtocolAdapter;
use crate::cmd::dap_server::server::configuration::ConsoleLog;
use crate::cmd::dap_server::server::configuration::CoreConfig;
use crate::cmd::dap_server::server::configuration::FlashingConfig;
use crate::cmd::dap_server::server::configuration::SessionConfig;
use crate::cmd::dap_server::server::debugger::Debugger;
use crate::rpc::client::RpcClient;
use crate::util::cli::Prompt;
use crate::util::rtt::RttConfig;
use crate::{CoreOptions, util::common_options::ProbeOptions};

use super::dap_server::debug_adapter::dap::dap_types::Request;
use super::dap_server::debug_adapter::dap::dap_types::Response;

/// A barebones adapter for the CLI "client".
struct CliAdapter {
    req_receiver: Receiver<Request>,
    msg_sender: Sender<(String, Option<serde_json::Value>)>,
    console_log_level: ConsoleLog,
    seq: i64,
    pending: HashMap<i64, Request>,
}

struct RttChannelInfo {
    name: String,
    prefix: String,
}

impl ProtocolAdapter for CliAdapter {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        if let Ok(msg) = self.req_receiver.try_recv() {
            self.pending.insert(msg.seq, msg.clone());
            return Ok(Some(msg));
        }

        Ok(None)
    }

    fn dyn_send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.msg_sender
            .try_send((event_type.to_string(), event_body))
            .unwrap();

        Ok(())
    }

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
        self.msg_sender
            .try_send((
                "response".to_string(),
                Some(serde_json::to_value(response)?),
            ))
            .unwrap();

        Ok(())
    }

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String> {
        self.pending.remove(&request_seq).map(|r| r.command)
    }

    fn set_console_log_level(&mut self, log_level: ConsoleLog) {
        self.console_log_level = log_level;
    }

    fn console_log_level(&self) -> ConsoleLog {
        self.console_log_level
    }

    fn get_next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }
}

fn error_response_to_string(response: Response) -> String {
    if response.message.as_deref() != Some("cancelled") {
        // `ProtocolHelper::send_response` sets `cancelled` as the message.
        return format!(
            "Error while processing {} - unexpected response: {:?}",
            response.command, response.message
        );
    }

    let Some(body) = response.body.clone() else {
        // `ProtocolHelper::send_response` sets an error body.
        return format!(
            "Unspecified error while processing {} - response has no body",
            response.command
        );
    };

    let Ok(response_body) = serde_json::from_value::<ErrorResponseBody>(body) else {
        // `ProtocolHelper::send_response` sets an error body.
        return format!(
            "Unspecified error while processing {} - failed to deserialize response body",
            response.command
        );
    };

    let Some(error) = response_body.error else {
        // `ProtocolHelper::send_response` returns some error to us.
        return format!(
            "Unspecified error while processing {} - response body has no error.",
            response.command
        );
    };

    format!("Error while processing {}: {}", response.command, error)
}

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    /// Binary to debug (ELF file). If provided with --launch, the binary will be flashed to the target.
    #[clap(value_parser)]
    pub binary: Option<PathBuf>,

    /// Launch instead of just attaching. This will reset the target and allow the binary to be flashed, if provided.
    #[clap(long)]
    pub launch: bool,

    /// Disable reset vector catch if its supported on the target.
    #[clap(long)]
    pub no_catch_reset: bool,

    /// Disable hardfault vector catch if its supported on the target.
    #[clap(long)]
    pub no_catch_hardfault: bool,

    /// Disable reading RTT data.
    #[clap(long, help_heading = "LOG CONFIGURATION / RTT")]
    pub no_rtt: bool,

    // TODO: support all options in BinaryDownloadOptions
    /// Before flashing, read back all the flashed data to skip flashing if the device is up to date.
    #[arg(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub preverify: bool,

    /// After flashing, read back all the flashed data to verify it has been written correctly.
    #[arg(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub verify: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient, utc_offset: UtcOffset) -> anyhow::Result<()> {
        let (req_sender, req_receiver) = mpsc::channel(10);
        let (msg_sender, mut msg_receiver) = mpsc::channel(10);

        let debug_adapter = DebugAdapter::new(CliAdapter {
            req_receiver,
            msg_sender: msg_sender.clone(),
            console_log_level: ConsoleLog::Console,
            seq: 0,
            pending: HashMap::new(),
        });

        let mut debug_client = Client {
            writer: None,
            rtt_channels: HashMap::new(),
            req_sender,
            seq: 0,
            is_initialized: false,
            is_terminated: false,
        };

        debug_client
            .send(|seq| Request {
                command: "initialize".to_string(),
                arguments: serde_json::to_value(&InitializeRequestArguments {
                    adapter_id: "probe-rs-cli".to_string(),
                    client_id: Some("probe-rs-cli".to_string()),
                    client_name: Some("probe-rs command-line debugger".to_string()),
                    columns_start_at_1: None,
                    lines_start_at_1: None,
                    locale: None,
                    path_format: None,
                    supports_args_can_be_interpreted_by_shell: None,
                    supports_invalidated_event: None,
                    supports_memory_event: None,
                    supports_memory_references: None,
                    supports_progress_reporting: Some(true),
                    supports_run_in_terminal_request: None,
                    supports_start_debugging_request: None,
                    supports_variable_paging: None,
                    supports_variable_type: None,
                    supports_ansi_styling: None,
                })
                .ok(),
                seq,
                type_: "request".to_string(),
            })
            .await;

        // Determine if this is a launch or attach session
        let session_command = if self.launch { "launch" } else { "attach" };

        debug_client
            .send(|seq| Request {
                command: session_command.to_string(),
                arguments: serde_json::to_value(&SessionConfig {
                    console_log_level: None,
                    cwd: None,
                    probe: self.common.probe,
                    chip: self.common.chip,
                    chip_description_path: self.common.chip_description_path,
                    connect_under_reset: self.common.connect_under_reset,
                    speed: self.common.speed,
                    wire_protocol: self.common.protocol,
                    allow_erase_all: false,
                    flashing_config: FlashingConfig {
                        flashing_enabled: self.launch && self.binary.is_some(),
                        verify_before_flashing: self.preverify,
                        verify_after_flashing: self.verify,
                        ..FlashingConfig::default()
                    },
                    core_configs: vec![CoreConfig {
                        core_index: self.shared.core,
                        program_binary: self.binary.clone(),
                        svd_file: None,
                        rtt_config: RttConfig {
                            enabled: !self.no_rtt,
                            channels: vec![],
                            default_config: Default::default(),
                        },
                        catch_hardfault: !self.no_catch_hardfault,
                        catch_reset: !self.no_catch_reset,
                    }],
                })
                .ok(),
                seq,
                type_: "request".to_string(),
            })
            .await;

        debug_client
            .send(|seq| Request {
                command: "configurationDone".to_string(),
                arguments: serde_json::to_value(()).ok(),
                seq,
                type_: "request".to_string(),
            })
            .await;

        // Run the debugger in a separate thread, otherwise longer processes like flashing can block the terminal.
        let server = tokio::task::spawn_blocking(move || {
            Runtime::new().unwrap().block_on(async move {
                let registry = &mut *client.registry().await;
                let mut debugger = Debugger::new(utc_offset, None)?;

                let lister = Lister::new();
                debugger
                    .debug_session(registry, debug_adapter, &lister)
                    .await
            })
        });

        while !debug_client.is_initialized && debug_client.running() {
            tokio::select! {
                response = msg_receiver.recv() => {
                    // Handle responses/events
                    let Some((event, body)) = response else {
                        // recv returns `None` if the channel has been closed
                        break;
                    };
                    debug_client.process_event(&event, body)?;
                }
            }
        }

        let (mut rl, writer) = Readline::new(Prompt("Debug Console> ").to_string()).unwrap();

        debug_client.writer = Some(writer);

        let readline = async {
            let mut result = Ok(());
            while debug_client.running() {
                tokio::select! {
                    response = msg_receiver.recv() => {
                        // Handle responses/events
                        let Some((event, body)) = response else {
                            // recv returns `None` if the channel has been closed
                            break;
                        };
                        if let Err(error) = debug_client.process_event(&event, body) {
                            result = Err(error);
                            break;
                        }
                    },
                    read_line = rl.readline() => {
                        match read_line {
                            Ok(ReadlineEvent::Line(line)) => {
                                rl.add_history_entry(line.clone());

                                debug_client
                                    .send(|seq| Request {
                                        command: "evaluate".to_string(),
                                        arguments: serde_json::to_value(&EvaluateArguments {
                                            context: Some("repl".to_string()),
                                            expression: line,
                                            format: None,
                                            frame_id: None,
                                            column: None,
                                            line: None,
                                            source: None,
                                        })
                                        .ok(),
                                        seq,
                                        type_: "request".to_string(),
                                    })
                                    .await;
                            }
                            // For end of file and ctrl-c, we just quit
                            Ok(ReadlineEvent::Eof | ReadlineEvent::Interrupted) => break,
                            Err(actual_error) => {
                                result = Err(actual_error).context("Error handling input");
                                break;
                            }
                        }
                    },
                    _ = debug_client.req_sender.closed() => break,
                }
            }

            debug_client
                .send(|seq| Request {
                    command: "disconnect".to_string(),
                    arguments: serde_json::to_value(&DisconnectArguments {
                        restart: None,
                        suspend_debuggee: Some(true),
                        terminate_debuggee: None,
                    })
                    .ok(),
                    seq,
                    type_: "request".to_string(),
                })
                .await;
            result
        };

        let (readline_result, server_result) = tokio::join! {
            readline,
            server,
        };

        rl.flush()?;
        server_result??;

        readline_result
    }
}

struct Client {
    writer: Option<SharedWriter>,
    rtt_channels: HashMap<u32, RttChannelInfo>,
    req_sender: Sender<Request>,

    is_initialized: bool,
    is_terminated: bool,

    /// Request sequence counter.
    seq: i64,
}

impl Client {
    fn next_seq(&mut self) -> i64 {
        let r = self.seq;
        self.seq += 1;
        r
    }

    async fn send(&mut self, cb: impl FnOnce(i64) -> Request) {
        let next_seq = self.next_seq();
        _ = self.req_sender.send(cb(next_seq)).await;
    }

    fn write_to_cli(&mut self, message: impl AsRef<str>) {
        let trimmed = message.as_ref().trim_end();
        if !trimmed.is_empty() {
            // Shared writer only flushes whole lines
            self.write_raw_to_cli(trimmed);
            self.write_raw_to_cli("\n");
        }
    }

    fn write_raw_to_cli(&mut self, message: impl AsRef<str>) {
        if let Some(writer) = self.writer.as_mut() {
            write!(writer, "{}", message.as_ref()).unwrap();
        } else {
            print!("{}", message.as_ref());
        }
    }

    fn update_rtt_prefixes(&mut self) {
        let channel_count = self.rtt_channels.len();
        let max_width = self
            .rtt_channels
            .values()
            .map(|info| info.name.len())
            .max()
            .unwrap_or(0);

        for info in self.rtt_channels.values_mut() {
            info.prefix = if channel_count > 1 {
                format!("[{:width$}] ", info.name, width = max_width)
            } else {
                String::new()
            };
        }
    }

    fn process_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        match event_type {
            "response" => {
                let response = serde_json::from_value::<Response>(event_body.unwrap())?;
                match response.command.as_str() {
                    _ if !response.success => {
                        anyhow::bail!("{}", error_response_to_string(response));
                    }

                    "evaluate" => {
                        let Some(body) = response.body else {
                            unreachable!();
                        };
                        let body = serde_json::from_value::<EvaluateResponseBody>(body)?;

                        self.write_to_cli(body.result);
                    }
                    "initialize" => {}
                    "configurationDone" => self.is_initialized = true,
                    "attach" => {}
                    _ => tracing::debug!("{response:?}"),
                }
            }

            "output" => {
                let Some(body) = event_body else {
                    return Ok(());
                };

                let output = serde_json::from_value::<OutputEventBody>(body)?;

                self.write_to_cli(output.output);
            }
            "probe-rs-show-message" => {
                let Some(body) = event_body else {
                    return Ok(());
                };

                let output = serde_json::from_value::<ShowMessageEventBody>(body)?;

                self.write_to_cli(output.message);
            }
            // Sent for the "quit" command, exits the readline Future and triggers a disconnection.
            "terminated" => self.is_terminated = true,
            // Not interesting
            "memory" => {}
            "stopped" => {}
            "breakpoint" => {}
            "probe-rs-rtt-channel-config" => {
                let Some(body) = event_body else {
                    return Ok(());
                };

                let output = serde_json::from_value::<RttChannelEventBody>(body)?;
                let entry = self
                    .rtt_channels
                    .entry(output.channel_number)
                    .or_insert_with(|| RttChannelInfo {
                        name: output.channel_name.clone(),
                        prefix: String::new(),
                    });
                entry.name = output.channel_name;
                self.update_rtt_prefixes();

                let request = Request {
                    command: "rttWindowOpened".to_string(),
                    arguments: serde_json::to_value(&RttWindowOpenedArguments {
                        channel_number: output.channel_number,
                        window_is_open: true,
                    })
                    .ok(),
                    seq: self.next_seq(),
                    type_: "request".to_string(),
                };

                if let Err(error) = self.req_sender.try_send(request) {
                    tracing::debug!("Failed to send rttWindowOpened request: {error}");
                }
            }
            "probe-rs-rtt-data" => {
                let Some(body) = event_body else {
                    return Ok(());
                };

                let output = serde_json::from_value::<RttDataEventBody>(body)?;
                let prefix = self
                    .rtt_channels
                    .get(&output.channel_number)
                    .map(|info| info.prefix.as_str())
                    .unwrap_or("");
                let message = format!("{prefix}{}", output.data);
                self.write_raw_to_cli(message);
            }
            // TODO: print a progress bar
            "progressStart" => {
                let Some(body) = event_body else {
                    return Ok(());
                };
                let event = serde_json::from_value::<ProgressStartEventBody>(body)?;

                self.write_to_cli(&event.title);
            }
            "progressUpdate" => {
                let Some(body) = event_body else {
                    return Ok(());
                };
                let event = serde_json::from_value::<ProgressUpdateEventBody>(body)?;

                if let Some(message) = event.message {
                    self.write_to_cli(message);
                }
            }
            "progressEnd" => {}
            // We can safely ignore "exited"
            "exited" => {}
            _ => tracing::debug!("Unhandled event {event_type}: {event_body:?}"),
        }

        Ok(())
    }

    fn running(&self) -> bool {
        !self.is_terminated
    }
}
