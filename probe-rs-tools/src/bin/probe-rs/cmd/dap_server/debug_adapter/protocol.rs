use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::dap_types::{
        ErrorResponseBody, Event, Message, MessageSeverity, OutputEventBody, ProtocolMessage,
        Request, Response, ShowMessageEventBody,
    },
    server::configuration::ConsoleLog,
};
use anyhow::{Context, anyhow};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader, Read, Write},
    str,
};
use tokio::sync::mpsc::{Receiver, channel};
use tokio_util::{bytes::BytesMut, codec::Decoder};
use tracing::instrument;

use super::codec::decoder::{Frame, MsDapDecoder};

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    async fn listen(&mut self);

    async fn receiver(&mut self) -> &mut Receiver<Request>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>;

    fn send_raw_response(&mut self, response: &Response) -> anyhow::Result<()>;

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String>;

    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn console_log_level(&self) -> ConsoleLog;
}

pub trait ProtocolHelper {
    fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool;

    /// Log a message to the console. Returns false if logging the message failed.
    fn log_to_console(&mut self, message: impl Into<String>) -> bool;

    fn send_response<S: Serialize + std::fmt::Debug>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, &DebuggerError>,
    ) -> Result<(), anyhow::Error>;
}

impl<P> ProtocolHelper for P
where
    P: ProtocolAdapter,
{
    fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool {
        let msg = message.into();

        tracing::debug!("show_message: {msg}");

        let event_body = match serde_json::to_value(ShowMessageEventBody {
            severity,
            message: format!("{}\n", msg),
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.send_event("probe-rs-show-message", Some(event_body))
            .is_ok()
    }

    fn log_to_console(&mut self, message: impl Into<String>) -> bool {
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
        self.send_event("output", Some(event_body)).is_ok()
    }

    fn send_response<S: Serialize + std::fmt::Debug>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, &DebuggerError>,
    ) -> Result<(), anyhow::Error> {
        let response_is_ok = response.is_ok();

        // The encoded response will be constructed from dap::Response for Ok, and dap::ErrorResponse for Err, to ensure VSCode doesn't lose the details of the error.
        let encoded_resp = match response {
            Ok(value) => Response {
                command: request.command.clone(),
                request_seq: request.seq,
                seq: request.seq,
                success: true,
                type_: "response".to_owned(),
                message: None,
                body: value.map(|v| serde_json::to_value(v)).transpose()?,
            },
            Err(debugger_error) => {
                let mut response_message = debugger_error.to_string();
                let mut offset_iterations = 0;
                let mut child_error: Option<&dyn std::error::Error> =
                    std::error::Error::source(&debugger_error);
                while let Some(source_error) = child_error {
                    offset_iterations += 1;
                    response_message = format!("{response_message}\n",);
                    for _offset_counter in 0..offset_iterations {
                        response_message = format!("{response_message}\t");
                    }
                    response_message = format!(
                        "{}{:?}",
                        response_message,
                        <dyn std::error::Error>::to_string(source_error)
                    );
                    child_error = std::error::Error::source(source_error);
                }
                // We have to send log messages on error conditions to the DAP Client now, because
                // if this error happens during the 'launch' or 'attach' request, the DAP Client
                // will not initiate a session, and will not be listening for 'output' events.
                self.log_to_console(&response_message);

                let response_body = ErrorResponseBody {
                    error: Some(Message {
                        format: "{response_message}".to_string(),
                        variables: Some(BTreeMap::from([(
                            "response_message".to_string(),
                            response_message,
                        )])),
                        // TODO: Implement unique error codes, that can index into the documentation for more information and suggested actions.
                        id: 0,
                        send_telemetry: Some(false),
                        show_user: Some(true),
                        url_label: Some("Documentation".to_string()),
                        url: Some("https://probe.rs/docs/tools/debugger/".to_string()),
                    }),
                };

                Response {
                    command: request.command.clone(),
                    request_seq: request.seq,
                    seq: request.seq,
                    success: false,
                    type_: "response".to_owned(),
                    message: Some("cancelled".to_string()), // Predefined value in the MSDAP spec.
                    body: Some(serde_json::to_value(response_body)?),
                }
            }
        };

        tracing::debug!("send_response: {:?}", encoded_resp);

        // Check if we got a request for this response
        if let Some(request_command) = self.remove_pending_request(request.seq) {
            assert_eq!(request_command, request.command);
        } else {
            tracing::error!(
                "Trying to send a response to non-existing request! {:?} has no pending request",
                encoded_resp
            );
        }

        self.send_raw_response(&encoded_resp)
            .context("Unexpected Error while sending response.")?;

        if response_is_ok {
            match self.console_log_level() {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(format!(
                        "   Sent DAP Response sequence #{} : {}",
                        request.seq, request.command
                    ));
                }
                ConsoleLog::Debug => {
                    self.log_to_console(format!(
                        "\nSent DAP Response: {:#?}",
                        serde_json::to_value(encoded_resp)?
                    ));
                }
            }
        }

        Ok(())
    }
}

pub struct DapAdapter<R: Read, W: Write> {
    input: BufReader<R>,
    output: W,
    console_log_level: ConsoleLog,
    seq: i64,

    pending_requests: HashMap<i64, String>,
}

impl<R: Read, W: Write> DapAdapter<R, W> {
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self {
            input: BufReader::new(reader),
            output: writer,
            seq: 1,
            console_log_level: ConsoleLog::Console,
            pending_requests: HashMap::new(),
        }
    }

    #[instrument(level = "trace", skip_all)]
    fn send_data(&mut self, raw_data: &[u8]) -> Result<(), std::io::Error> {
        let mut response_body = raw_data;

        let response_header = format!("Content-Length: {}\r\n\r\n", response_body.len());

        self.output.write_all(response_header.as_bytes())?;
        self.output.flush()?;

        // NOTE: Sometimes when writing large response, the debugger will fail with an IO error (ErrorKind::WouldBlock == error.kind())
        let mut bytes_remaining = response_body.len();
        while bytes_remaining > 0 {
            match self.output.write(response_body) {
                Ok(bytes_written) => {
                    bytes_remaining = bytes_remaining.saturating_sub(bytes_written);
                    response_body = &response_body[bytes_written..];
                }
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        // The client is not ready to receive data (probably still processing the last chunk we sent),
                        // so we need to keep trying.
                    } else {
                        tracing::error!("Failed to send a response to the client: {}", error);
                        return Err(error);
                    }
                }
            }
        }
        self.output.flush()?;

        self.seq += 1;

        Ok(())
    }
}

impl<R: Read + Send + Sync, W: Write + Send + Sync> ProtocolAdapter for DapAdapter<R, W> {
    async fn receive(mut self) -> &mut Receiver<Request> {
        let (sender, receiver) = channel(256);
        let mut decoder = MsDapDecoder::<Request>::new();
        tokio::spawn(async move {
            let mut buf = BytesMut::new();
            loop {
                loop {
                    match decoder.decode(&mut buf) {
                        Ok(Some(Frame { content: request })) => {
                            tracing::debug!("Received request: {:?}", request);

                            // This is the SUCCESS request for new requests from the client.
                            match self.console_log_level {
                                ConsoleLog::Console => {}
                                ConsoleLog::Info => {
                                    self.log_to_console(format!(
                                        "\nReceived DAP Request sequence #{} : {}",
                                        request.seq, request.command
                                    ));
                                }
                                ConsoleLog::Debug => {
                                    self.log_to_console(format!(
                                        "\nReceived DAP Request: {request:#?}"
                                    ));
                                }
                            }

                            sender.send(request);
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("Error while listening to request: {:?}", e);
                            self.log_to_console(e.to_string());
                            self.show_message(MessageSeverity::Error, e.to_string());
                        }
                    }
                }
            }
        });
        receiver
    }

    #[instrument(level = "trace", skip_all)]
    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()> {
        let new_event = Event {
            seq: self.seq,
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body.map(|event_body| serde_json::to_value(event_body).unwrap_or_default()),
        };

        let encoded_event = serde_json::to_vec(&new_event)?;

        let result = self
            .send_data(&encoded_event)
            .context("Unexpected Error while sending event.");

        if new_event.event != "output" {
            // This would result in an endless loop.
            match self.console_log_level {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(format!("\nTriggered DAP Event: {}", new_event.event));
                }
                ConsoleLog::Debug => {
                    self.log_to_console(format!("INFO: Triggered DAP Event: {new_event:#?}"));
                }
            }
        }

        result
    }

    fn set_console_log_level(&mut self, log_level: ConsoleLog) {
        self.console_log_level = log_level;
    }

    fn console_log_level(&self) -> ConsoleLog {
        self.console_log_level
    }

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String> {
        self.pending_requests.remove(&request_seq)
    }

    fn send_raw_response(&mut self, response: &Response) -> anyhow::Result<()> {
        let encoded_response = serde_json::to_vec(&response)?;

        self.send_data(&encoded_response)?;

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use std::io::{self, ErrorKind};

    use super::*;

    struct TestReader {
        response: Option<io::Result<usize>>,
    }

    impl Read for TestReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            if let Some(response) = self.response.take() {
                response
            } else {
                Err(io::Error::other("Repeated use of test reader"))
            }
        }
    }

    struct FailingWriter {}

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("FailingWriter"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("FailingWriter"))
        }
    }

    #[test]
    fn event_send_error() {
        let mut adapter = DapAdapter::new(io::empty(), FailingWriter {});

        let result = adapter.send_event("probe-rs-test", Some(()));

        assert!(result.is_err());
    }

    #[test]
    fn message_send_error() {
        let mut adapter = DapAdapter::new(io::empty(), FailingWriter {});

        let result = adapter.show_message(MessageSeverity::Error, "probe-rs-test");

        assert!(!result);
    }
}
