use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;
use rustyline::{DefaultEditor, error::ReadlineError};
use time::UtcOffset;
use tokio::sync::mpsc::Receiver;

use crate::cmd::dap_server::debug_adapter::dap::adapter::DebugAdapter;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::ErrorResponseBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::EvaluateArguments;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::EvaluateResponseBody;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::InitializeRequestArguments;
use crate::cmd::dap_server::debug_adapter::dap::dap_types::OutputEventBody;
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

struct Shared {
    stop: bool,
    next_request: Option<Request>,
}

/// A barebones adapter for the CLI "client".
struct CliAdapter {
    shared: Rc<RefCell<Shared>>,
    console_log_level: ConsoleLog,
}
impl ProtocolAdapter for CliAdapter {
    async fn subscribe_requests(&mut self) -> Receiver<anyhow::Result<Request>> {}
    fn send_event<S: serde::Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()> {
        let serialized_body = event_body.as_ref().map(serde_json::to_string).transpose()?;

        match event_type {
            "probe-rs-show-message" | "output" => {
                let Some(body) = serialized_body else {
                    return Ok(());
                };

                let output = serde_json::from_str::<OutputEventBody>(&body)?;

                print!("{}", output.output);
            }
            "terminated" => {
                self.shared.borrow_mut().stop = true;
            }
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
            _ => {
                tracing::warn!("Unhandled event {event_type}: {serialized_body:?}");
            }
        }

        Ok(())
    }

    fn send_raw_response(&mut self, response: &Response) -> anyhow::Result<()> {
        if !response.success {
            print_error(response)?;
            return Ok(());
        }

        match response.command.as_str() {
            "evaluate" => {
                let Some(body) = &response.body else {
                    unreachable!();
                };
                let body = serde_json::from_value::<EvaluateResponseBody>(body.clone())?;

                println!("{}", body.result);
            }
            "initialize" => {}
            "attach" => {}
            _ => println!("{response:?}"),
        }

        Ok(())
    }

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String> {
        self.shared.borrow_mut().next_request.take().and_then(|r| {
            if request_seq == r.seq {
                Some(r.command)
            } else {
                None
            }
        })
    }

    fn set_console_log_level(&mut self, log_level: ConsoleLog) {
        self.console_log_level = log_level;
    }

    fn console_log_level(&self) -> ConsoleLog {
        self.console_log_level
    }
}

fn print_error(response: &Response) -> anyhow::Result<()> {
    if response.message.as_deref() != Some("cancelled") {
        // `ProtocolHelper::send_response` sets `cancelled` as the message.
        println!(
            "Error while processing {} - unexpected response: {:?}",
            response.command, response.message
        );
        return Ok(());
    }

    let Some(body) = response.body.clone() else {
        // `ProtocolHelper::send_response` sets an error body.
        println!(
            "Unspecified error while processing {} - response has no body",
            response.command
        );
        return Ok(());
    };

    let response_body = serde_json::from_value::<ErrorResponseBody>(body)?;

    let Some(error) = response_body.error else {
        // `ProtocolHelper::send_response` returns some error to us.
        println!(
            "Unspecified error while processing {} - response body has no error.",
            response.command
        );
        return Ok(());
    };

    println!("Error while processing {}: {}", response.command, error);

    Ok(())
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
        let shared = Rc::new(RefCell::new(Shared {
            stop: false,
            next_request: None,
        }));
        let mut debug_adapter = DebugAdapter::new(CliAdapter {
            shared: shared.clone(),
            console_log_level: ConsoleLog::Console,
        });
        let mut debugger = Debugger::new(utc_offset, None)?;

        shared.borrow_mut().next_request = Some(Request {
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
            })
            .ok(),
            seq: 0,
            type_: "request".to_string(),
        });
        debugger.handle_initialize(&mut debug_adapter)?;

        let attach_request = Request {
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
                    },
                    catch_hardfault: !self.no_catch_hardfault,
                    catch_reset: !self.no_catch_reset,
                }],
            })
            .ok(),
            seq: 1,
            type_: "request".to_string(),
        };

        // A bit weird since we need the request to be removable,
        // but we also need to pass it directly.
        shared.borrow_mut().next_request = Some(attach_request.clone());
        let mut session_data = debugger
            .handle_launch_attach(registry, &attach_request, &mut debug_adapter, lister)
            .await?;

        shared.borrow_mut().next_request = Some(Request {
            command: "configurationDone".to_string(),
            arguments: serde_json::to_value(()).ok(),
            seq: 2,
            type_: "request".to_string(),
        });
        debugger
            .process_next_request(&mut session_data, &mut debug_adapter)
            .await?;

        let mut rl = DefaultEditor::new()?;

        let mut seq = 3;
        while !shared.borrow().stop {
            match rl.readline(">> ") {
                Ok(line) => {
                    rl.add_history_entry(&line)?;

                    let request = Request {
                        command: "evaluate".to_string(),
                        arguments: serde_json::to_value(&EvaluateArguments {
                            context: Some("repl".to_string()),
                            expression: line,
                            format: None,
                            frame_id: None,
                        })
                        .ok(),
                        seq: {
                            let s = seq;
                            seq += 1;
                            s
                        },
                        type_: "request".to_string(),
                    };

                    shared.borrow_mut().next_request = Some(request);
                    debugger
                        .process_next_request(&mut session_data, &mut debug_adapter)
                        .await?;
                }
                // For end of file and ctrl-c, we just quit
                Err(ReadlineError::Eof | ReadlineError::Interrupted) => return Ok(()),
                Err(actual_error) => {
                    // Show error message and quit
                    println!("Error handling input: {actual_error:?}");
                    break;
                }
            }
        }

        Ok(())
    }
}
