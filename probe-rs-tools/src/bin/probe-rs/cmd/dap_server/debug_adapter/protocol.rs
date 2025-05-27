use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::dap_types::{
        ErrorResponseBody, Event, MessageSeverity, OutputEventBody, Request, Response,
        ShowMessageEventBody,
    },
    server::configuration::ConsoleLog,
};
use anyhow::{Context, anyhow};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    str,
};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, Encoder},
};
use tracing::instrument;

use super::codec::{DapCodec, Frame, Message};

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>;

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()>;

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String>;

    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn console_log_level(&self) -> ConsoleLog;

    /// Increases the sequence number by 1 and returns it.
    fn get_next_seq(&mut self) -> i64;
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
                seq: self.get_next_seq(),
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
                    error: Some(super::dap::dap_types::Message {
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
                    seq: self.get_next_seq(),
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

        self.send_raw_response(encoded_resp.clone())
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

    codec: DapCodec<Message>,
    input_buffer: BytesMut,
}

impl<R: Read, W: Write> DapAdapter<R, W> {
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self {
            input: BufReader::new(reader),
            output: writer,
            seq: 0,
            console_log_level: ConsoleLog::Console,
            pending_requests: HashMap::new(),

            codec: DapCodec::new(),
            input_buffer: BytesMut::with_capacity(4096),
        }
    }

    #[instrument(level = "trace", skip_all)]
    fn send_data(&mut self, item: Frame<Message>) -> Result<(), std::io::Error> {
        let mut buf = BytesMut::with_capacity(4096);
        self.codec.encode(item, &mut buf)?;
        self.output.write_all(&buf)?;
        self.output.flush()?;
        Ok(())
    }

    /// Receive data from `self.input`. Data has to be in the format specified by the Debug Adapter Protocol (DAP).
    /// The returned data is the content part of the request, as raw bytes.
    fn receive_data(&mut self) -> Result<Option<Frame<Message>>, DebuggerError> {
        match self.input.fill_buf() {
            Ok(data) => {
                // New data is here. Shove it into the buffer.
                self.input_buffer.extend_from_slice(data);
                let consumed = data.len();
                self.input.consume(consumed);
            }
            Err(error) => match error.kind() {
                // No new data is here and we also have nothing buffered, go back to polling.
                ErrorKind::WouldBlock if self.input_buffer.is_empty() => return Ok(None),
                // No new data is here but we have some buffered, so go to work the data and produce frames.
                ErrorKind::WouldBlock if !self.input_buffer.is_empty() => {}
                // An error ocurred, report it.
                _ => return Err(error.into()),
            },
        };

        // Process the next message from the buffer.
        Ok(self.codec.decode(&mut self.input_buffer)?)
    }

    fn listen_for_request_and_respond(&mut self) -> anyhow::Result<Option<Request>> {
        match self.receive_msg_content() {
            Ok(Some(request)) => {
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
                        self.log_to_console(format!("\nReceived DAP Request: {request:#?}"));
                    }
                }

                // Store pending request for debugging purposes
                self.pending_requests
                    .insert(request.seq, request.command.clone());

                Ok(Some(request))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                tracing::warn!("Error while listening to request: {:?}", e);
                self.log_to_console(e.to_string());
                self.show_message(MessageSeverity::Error, e.to_string());

                Err(anyhow!(e))
            }
        }
    }

    fn receive_msg_content(&mut self) -> Result<Option<Request>, DebuggerError> {
        match self.receive_data() {
            Ok(Some(frame)) => {
                // Extract protocol message
                if let Message::Request(request) = frame.content {
                    Ok(Some(request))
                } else {
                    Err(DebuggerError::Other(anyhow!(
                        "Received an unexpected message type: '{:?}'",
                        frame.content.kind()
                    )))
                }
            }
            Ok(None) => Ok(None),
            Err(error) => {
                // This is a legitimate error. Tell the client about it.
                Err(DebuggerError::Other(anyhow!("{}", error)))
            }
        }
    }
}

impl<R: Read, W: Write> ProtocolAdapter for DapAdapter<R, W> {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.listen_for_request_and_respond()
    }

    #[instrument(level = "trace", skip_all)]
    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()> {
        let new_event = Event {
            seq: self.get_next_seq(),
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body.map(|event_body| serde_json::to_value(event_body).unwrap_or_default()),
        };

        let result = self
            .send_data(Frame::new(new_event.clone().into()))
            .context("Unexpected Error while sending event.");

        if event_type != "output" {
            // This would result in an endless loop.
            match self.console_log_level {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(format!("\nTriggered DAP Event: {}", event_type));
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

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
        self.send_data(Frame::new(Message::Response(response)))?;

        Ok(())
    }

    fn get_next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
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

    #[test]
    fn receive_valid_request() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test\" }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let mut output = Vec::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), &mut output);
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().unwrap().unwrap();

        let output_str = String::from_utf8(output).unwrap();

        insta::assert_snapshot!(output_str);

        assert_eq!(request.command, "test");
        assert_eq!(request.seq, 3);
    }

    #[test]
    fn receive_request_with_invalid_json() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let mut output = Vec::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), &mut output);
        adapter.console_log_level = super::ConsoleLog::Info;

        let _request = adapter.listen_for_request().unwrap_err();

        let output_str = String::from_utf8(output).unwrap();

        insta::assert_snapshot!(output_str);
    }

    #[test]
    fn receive_request_would_block() {
        let input = TestReader {
            response: Some(io::Result::Err(io::Error::new(
                ErrorKind::WouldBlock,
                "would block",
            ))),
        };

        let mut output = Vec::new();

        let mut adapter = DapAdapter::new(input, &mut output);
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().unwrap();

        let output_str = String::from_utf8(output).unwrap();

        insta::assert_snapshot!(output_str);

        assert!(request.is_none());
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
