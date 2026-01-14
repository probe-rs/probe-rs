use std::collections::HashMap;
use std::env::VarError;
use std::io::Write;
use std::path::PathBuf;

use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use ratatui::crossterm::style::Stylize;
use rustyline_async::SharedWriter;
use rustyline_async::{Readline, ReadlineEvent};
use time::UtcOffset;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;

use crate::cmd::dap_server::debug_adapter::dap::adapter::DebugAdapter;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::ErrorResponseBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::EvaluateResponseBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::InitializeRequestArguments;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::OutputEventBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::{
    DisconnectArguments, EvaluateArguments,
};
use crate::cmd::dap_server::debug_adapter::protocol::ProtocolAdapter;
use crate::cmd::dap_server::server::configuration::ConsoleLog;
use crate::cmd::dap_server::server::configuration::CoreConfig;
use crate::cmd::dap_server::server::configuration::FlashingConfig;
use crate::cmd::dap_server::server::configuration::SessionConfig;
use crate::cmd::dap_server::server::debugger::Debugger;
use crate::util::rtt::RttConfig;
use crate::{CoreOptions, util::common_options::ProbeOptions};

use super::dap_server::debug_adapter::dap::dap_types::Request;
use super::dap_server::debug_adapter::dap::dap_types::Response;

/// A barebones adapter for the CLI "client".
struct CliAdapter {
    receiver: Receiver<Request>,
    writer: SharedWriter,
    console_log_level: ConsoleLog,
    seq: i64,
    pending: HashMap<i64, Request>,
    cancellation: CancellationToken,
}

impl CliAdapter {
    fn write_to_cli(&mut self, message: impl AsRef<str>) {
        writeln!(self.writer, "{}", message.as_ref().trim_end()).unwrap();
    }
}

impl ProtocolAdapter for CliAdapter {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        if let Ok(msg) = self.receiver.try_recv() {
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
        let serialized_body = event_body.as_ref().map(serde_json::to_string).transpose()?;

        match event_type {
            "probe-rs-show-message" | "output" => {
                let Some(body) = serialized_body else {
                    return Ok(());
                };

                let output = serde_json::from_str::<OutputEventBody>(&body)?;

                self.write_to_cli(output.output);
            }
            // Sent for the "quit" command, exits the readline Future and triggers a disconnection.
            "terminated" => self.cancellation.cancel(),
            // Not interesting
            "memory" => {}
            "stopped" => {}
            "breakpoint" => {}
            // No RTT support (yet)
            "probe-rs-rtt-channel-config" | "probe-rs-rtt-data" => {}
            // No flashing support (yet)
            "progressStart" | "progressEnd" | "progressUpdate" => {}
            // We can safely ignore "exited"
            "exited" => {}
            _ => tracing::debug!("Unhandled event {event_type}: {serialized_body:?}"),
        }

        Ok(())
    }

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
        match response.command.as_str() {
            _ if !response.success => self.write_to_cli(error_response_to_string(response)),

            "evaluate" => {
                let Some(body) = &response.body else {
                    unreachable!();
                };
                let body = serde_json::from_value::<EvaluateResponseBody>(body.clone())?;

                self.write_to_cli(body.result);
            }
            "initialize" => {}
            "attach" => {}
            _ => tracing::debug!("{response:?}"),
        }

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

    #[clap(long, value_parser)]
    /// Binary to debug
    exe: Option<PathBuf>,

    /// Disable reset vector catch if its supported on the target.
    #[clap(long)]
    pub no_catch_reset: bool,

    /// Disable hardfault vector catch if its supported on the target.
    #[clap(long)]
    pub no_catch_hardfault: bool,
}

impl Cmd {
    pub async fn run(
        self,
        registry: &mut Registry,
        lister: &Lister,
        utc_offset: UtcOffset,
    ) -> anyhow::Result<()> {
        let (sender, receiver) = mpsc::channel(5);

        let prompt = if matches!(
            std::env::var("PROBE_RS_COLOR").as_deref(),
            Err(VarError::NotPresent) | Ok("true" | "1" | "yes" | "on")
        ) {
            "> ".bold().dark_green().to_string()
        } else {
            ">> ".to_string()
        };

        let (mut rl, mut writer) = Readline::new(prompt).unwrap();

        // TODO: properly introduce a response/event channel, react to terminated event
        let cancellation = CancellationToken::new();

        let debug_adapter = DebugAdapter::new(CliAdapter {
            receiver,
            writer: writer.clone(),
            console_log_level: ConsoleLog::Console,
            seq: 0,
            pending: HashMap::new(),
            cancellation: cancellation.clone(),
        });
        let mut debugger = Debugger::new(utc_offset, None)?;

        let mut seq = 0;
        let mut next_seq = move || {
            let r = seq;
            seq += 1;
            r
        };

        sender
            .send(Request {
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
                    supports_progress_reporting: None,
                    supports_run_in_terminal_request: None,
                    supports_start_debugging_request: None,
                    supports_variable_paging: None,
                    supports_variable_type: None,
                    supports_ansi_styling: None,
                })
                .ok(),
                seq: next_seq(),
                type_: "request".to_string(),
            })
            .await
            .unwrap();
        sender
            .send(Request {
                command: "attach".to_string(),
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
                    flashing_config: FlashingConfig::default(),
                    core_configs: vec![CoreConfig {
                        core_index: self.shared.core,
                        program_binary: self.exe,
                        svd_file: None,
                        rtt_config: RttConfig {
                            enabled: false,
                            channels: vec![],
                            default_config: Default::default(),
                        },
                        catch_hardfault: !self.no_catch_hardfault,
                        catch_reset: !self.no_catch_reset,
                    }],
                })
                .ok(),
                seq: next_seq(),
                type_: "request".to_string(),
            })
            .await
            .unwrap();
        sender
            .send(Request {
                command: "configurationDone".to_string(),
                arguments: serde_json::to_value(()).ok(),
                seq: next_seq(),
                type_: "request".to_string(),
            })
            .await
            .unwrap();

        let server = async {
            debugger
                .debug_session(registry, debug_adapter, lister)
                .await
                .ok();
        };

        let readline = async {
            loop {
                let read_line = tokio::select! {
                    line = rl.readline() => line,
                    _ = sender.closed() => break,
                    _ = cancellation.cancelled() => break,
                };
                match read_line {
                    Ok(ReadlineEvent::Line(line)) => {
                        rl.add_history_entry(line.clone());

                        let request = Request {
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
                            seq: next_seq(),
                            type_: "request".to_string(),
                        };

                        sender.send(request).await.unwrap();
                    }
                    // For end of file and ctrl-c, we just quit
                    Ok(ReadlineEvent::Eof | ReadlineEvent::Interrupted) => break,
                    Err(actual_error) => {
                        // Show error message and quit
                        writeln!(&mut writer, "Error handling input: {actual_error:?}").unwrap();
                        break;
                    }
                }
            }

            sender
                .send(Request {
                    command: "disconnect".to_string(),
                    arguments: serde_json::to_value(&DisconnectArguments {
                        restart: None,
                        suspend_debuggee: Some(true),
                        terminate_debuggee: None,
                    })
                    .ok(),
                    seq: next_seq(),
                    type_: "request".to_string(),
                })
                .await
                .ok(); // Ignore error in case the sender is disconnected
        };

        tokio::join! {
            readline,
            server,
        };

        rl.flush()?;

        Ok(())
    }
}
