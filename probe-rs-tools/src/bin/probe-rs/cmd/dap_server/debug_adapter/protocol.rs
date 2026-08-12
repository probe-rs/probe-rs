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
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    str,
    time::{Duration, Instant},
};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, Encoder},
};
use tracing::instrument;

use super::codec::{DapCodec, Frame, Message};

fn would_block(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::WouldBlock
}

/// A client that does not read for this long holds up the whole server.
const SLOW_CLIENT_WARNING: Duration = Duration::from_millis(500);

/// A message that stops for this long counts as abandoned. The server polls
/// the target again, and keeps the part that arrived.
const PARTIAL_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Give the client time to read from the connection, and report a client that
/// keeps the connection full.
fn wait_for_output(blocked_since: Instant, warned: &mut bool) {
    if !*warned && blocked_since.elapsed() > SLOW_CLIENT_WARNING {
        *warned = true;
        tracing::warn!(
            "The DAP client does not read from the connection. The server waits for it."
        );
    }
    std::thread::sleep(Duration::from_millis(1));
}

/// Request argument fields that hold a base64 copy of a file.
const FILE_PAYLOAD_FIELDS: [&str; 3] = ["programBinaryData", "svdFileData", "chipDescriptionData"];

/// A view of a [`Request`] for a log message or a console message, in which
/// each file payload shows as its size.
///
/// In `remoteServerMode` the launch arguments hold a base64 copy of the
/// program binary, and the client repeats these arguments in every `restart`
/// request. The [`fmt::Debug`] output of such a request is tens of megabytes.
/// To format it, and for the client to display it, takes long enough to look
/// like a freeze.
pub(crate) struct RequestSummary<'a>(pub(crate) &'a Request);

impl fmt::Debug for RequestSummary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("seq", &self.0.seq)
            .field("type_", &self.0.type_)
            .field("command", &self.0.command)
            .field(
                "arguments",
                &self.0.arguments.as_ref().map(hide_file_payloads),
            )
            .finish()
    }
}

/// Replace every [`FILE_PAYLOAD_FIELDS`] entry of `value` with its size.
///
/// Use this for every message that shows request arguments. See
/// [`RequestSummary`].
pub(crate) fn hide_file_payloads(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, value)| {
                    let value = if FILE_PAYLOAD_FIELDS.contains(&name.as_str()) {
                        Value::String(format!(
                            "<{} bytes>",
                            value.as_str().unwrap_or_default().len()
                        ))
                    } else {
                        hide_file_payloads(value)
                    };
                    (name.clone(), value)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(hide_file_payloads).collect()),
        value => value.clone(),
    }
}

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        self.dyn_send_event(
            event_type,
            event_body.map(|event_body| serde_json::to_value(event_body).unwrap_or_default()),
        )
    }

    fn dyn_send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()>;

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()>;

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String>;

    /// Returns `true` while the request with the given sequence number waits for a response.
    fn has_pending_request(&self, request_seq: i64) -> bool;

    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn console_log_level(&self) -> ConsoleLog;

    /// Increases the sequence number by 1 and returns it.
    fn get_next_seq(&mut self) -> i64;
}

/// Type-erased [`ProtocolAdapter`].
///
/// The debug adapter is generic over its transport in principle, but every
/// transport-generic function it appears in would otherwise be monomorphised
/// once per transport (TCP, stdio, CLI). Boxing keeps a single instantiation.
pub type BoxedAdapter = Box<dyn ProtocolAdapter + Send>;

impl ProtocolAdapter for BoxedAdapter {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        (**self).listen_for_request()
    }

    fn dyn_send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        (**self).dyn_send_event(event_type, event_body)
    }

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
        (**self).send_raw_response(response)
    }

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String> {
        (**self).remove_pending_request(request_seq)
    }

    fn has_pending_request(&self, request_seq: i64) -> bool {
        (**self).has_pending_request(request_seq)
    }

    fn set_console_log_level(&mut self, log_level: ConsoleLog) {
        (**self).set_console_log_level(log_level)
    }

    fn console_log_level(&self) -> ConsoleLog {
        (**self).console_log_level()
    }

    fn get_next_seq(&mut self) -> i64 {
        (**self).get_next_seq()
    }
}

pub trait ProtocolHelper {
    fn show_message(&mut self, severity: MessageSeverity, message: impl AsRef<str>) -> bool
    where
        Self: Sized,
    {
        self.dyn_show_message(severity, message.as_ref().to_string())
    }

    fn dyn_show_message(&mut self, severity: MessageSeverity, message: String) -> bool;

    /// Log a message to the console. Returns false if logging the message failed.
    fn log_to_console(&mut self, message: &str) -> bool
    where
        Self: Sized;

    fn send_response<S: Serialize + std::fmt::Debug>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, &DebuggerError>,
    ) -> Result<(), anyhow::Error>
    where
        Self: Sized;
}

impl<P> ProtocolHelper for P
where
    P: ProtocolAdapter + ?Sized,
{
    fn dyn_show_message(&mut self, severity: MessageSeverity, message: String) -> bool {
        tracing::debug!("show_message: {message}");

        match serde_json::to_value(ShowMessageEventBody {
            severity,
            message: format!("{message}\n"),
        }) {
            Ok(event_body) => self
                .dyn_send_event("probe-rs-show-message", Some(event_body))
                .is_ok(),
            Err(_) => false,
        }
    }

    fn log_to_console(&mut self, message: &str) -> bool
    where
        Self: Sized,
    {
        let event_body = match serde_json::to_value(OutputEventBody {
            output: format!("{message}\n"),
            category: Some("console".to_owned()),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
            data: None,
            group: Some("probe-rs-debug".to_owned()),
            location_reference: None,
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.dyn_send_event("output", Some(event_body)).is_ok()
    }

    fn send_response<S: Serialize + std::fmt::Debug>(
        &mut self,
        request: &Request,
        response: Result<Option<S>, &DebuggerError>,
    ) -> Result<(), anyhow::Error>
    where
        Self: Sized,
    {
        let response = match response {
            Ok(Some(response)) => Ok(Some(serde_json::to_value(response)?)),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };

        send_response(self, request, response)
    }
}

fn send_response(
    this: &mut (impl ProtocolAdapter + ProtocolHelper),
    request: &Request,
    response: Result<Option<serde_json::Value>, &DebuggerError>,
) -> Result<(), anyhow::Error> {
    let response_is_ok = response.is_ok();
    // The encoded response will be constructed from dap::Response for Ok, and dap::ErrorResponse for Err, to ensure VSCode doesn't lose the details of the error.

    let (body, message) = match response {
        Ok(body) => (body, None),
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
            this.log_to_console(&response_message);

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

            (
                Some(serde_json::to_value(response_body)?),
                Some("cancelled".to_string()), // Predefined value in the MSDAP spec.
            )
        }
    };

    let encoded_resp = Response {
        command: request.command.clone(),
        request_seq: request.seq,
        seq: this.get_next_seq(),
        success: response_is_ok,
        type_: "response".to_owned(),
        message,
        body,
    };

    tracing::debug!("send_response: {:?}", encoded_resp);

    // Check if we got a request for this response
    if let Some(request_command) = this.remove_pending_request(request.seq) {
        assert_eq!(request_command, request.command);
    } else {
        tracing::error!(
            "Trying to send a response to non-existing request! {:?} has no pending request",
            encoded_resp
        );
    }

    this.send_raw_response(encoded_resp.clone())
        .context("Unexpected Error while sending response.")?;

    if response_is_ok {
        match this.console_log_level() {
            ConsoleLog::Console => {}
            ConsoleLog::Info => {
                this.log_to_console(&format!(
                    "   Sent DAP Response sequence #{} : {}",
                    request.seq, request.command
                ));
            }
            ConsoleLog::Debug => {
                this.log_to_console(&format!(
                    "\nSent DAP Response: {:#?}",
                    serde_json::to_value(encoded_resp)?
                ));
            }
        }
    }

    Ok(())
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
            input: BufReader::with_capacity(64 * 1024, reader),
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
        self.write_all_slowly(&buf)?;

        let blocked_since = Instant::now();
        let mut warned = false;
        loop {
            match self.output.flush() {
                Err(error) if would_block(&error) => wait_for_output(blocked_since, &mut warned),
                result => return result,
            }
        }
    }

    /// Write the whole buffer, and wait while the output cannot take more.
    ///
    /// In TCP mode the socket is non-blocking, because the server polls the
    /// target while no request waits. [`Write::write_all`] on such a socket
    /// fails as soon as the send buffer is full, which happens when the client
    /// is slow to read. A partial message breaks the frame format of the
    /// protocol, and the failure ends the debug session.
    fn write_all_slowly(&mut self, mut buf: &[u8]) -> Result<(), std::io::Error> {
        let blocked_since = Instant::now();
        let mut warned = false;
        while !buf.is_empty() {
            match self.output.write(buf) {
                Ok(0) => return Err(std::io::Error::from(ErrorKind::WriteZero)),
                Ok(written) => buf = &buf[written..],
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if would_block(&error) => wait_for_output(blocked_since, &mut warned),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Receive data from `self.input`. Data has to be in the format specified by the Debug Adapter Protocol (DAP).
    /// The returned data is the content part of the request, as raw bytes.
    ///
    /// The loop reads until a message is complete, or until the input has no
    /// more data. One read per call is not enough: a request that carries a
    /// program binary is several megabytes long, and the caller reads again
    /// only after the next poll of the target.
    ///
    /// While a message is on its way, the loop waits for the rest of it
    /// instead of going back to the caller. The poll of the target holds the
    /// loop for 100 ms per turn, which limits the input to the size of the
    /// receive buffer of the socket per turn.
    fn receive_data(&mut self) -> Result<Option<Frame<Message>>, DebuggerError> {
        let mut idle_since = Instant::now();
        loop {
            if let Some(frame) = self.codec.decode(&mut self.input_buffer)? {
                return Ok(Some(frame));
            }

            match self.input.fill_buf() {
                // The input is at its end.
                Ok([]) => return Ok(None),
                Ok(data) => {
                    self.input_buffer.extend_from_slice(data);
                    let consumed = data.len();
                    self.input.consume(consumed);
                    idle_since = Instant::now();
                }
                Err(error) => match error.kind() {
                    ErrorKind::Interrupted => {}
                    // No part of a message waits, thus go back to polling.
                    ErrorKind::WouldBlock if self.input_buffer.is_empty() => return Ok(None),
                    // A client that stops in the middle of a message must not
                    // hold the poll of the target. Keep the part that arrived.
                    ErrorKind::WouldBlock if idle_since.elapsed() > PARTIAL_MESSAGE_TIMEOUT => {
                        return Ok(None);
                    }
                    ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                    _ => return Err(error.into()),
                },
            }
        }
    }

    fn listen_for_request_and_respond(&mut self) -> anyhow::Result<Option<Request>> {
        match self.receive_msg_content() {
            Ok(Some(request)) => {
                tracing::debug!("Received request: {:?}", RequestSummary(&request));

                // This is the SUCCESS request for new requests from the client.
                match self.console_log_level {
                    ConsoleLog::Console => {}
                    ConsoleLog::Info => {
                        self.log_to_console(&format!(
                            "\nReceived DAP Request sequence #{} : {}",
                            request.seq, request.command
                        ));
                    }
                    ConsoleLog::Debug => {
                        self.log_to_console(&format!(
                            "\nReceived DAP Request: {:#?}",
                            RequestSummary(&request)
                        ));
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
                self.log_to_console(&e.to_string());
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
                Err(DebuggerError::Other(anyhow!("{error}")))
            }
        }
    }
}

impl<R: Read, W: Write> ProtocolAdapter for DapAdapter<R, W> {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.listen_for_request_and_respond()
    }

    #[instrument(level = "trace", skip_all)]
    fn dyn_send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        tracing::debug!("Sending event: {}", event_type);

        let new_event = Event {
            seq: self.get_next_seq(),
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body,
        };

        if event_type != "output" {
            // This would result in an endless loop.
            match self.console_log_level {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(&format!("\nTriggered DAP Event: {event_type}"));
                }
                ConsoleLog::Debug => {
                    self.log_to_console(&format!("INFO: Triggered DAP Event: {new_event:#?}"));
                }
            }
        }

        self.send_data(Frame::new(new_event.into()))
            .context("Unexpected Error while sending event.")
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

    fn has_pending_request(&self, request_seq: i64) -> bool {
        self.pending_requests.contains_key(&request_seq)
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

    /// A reader that hands out the input in small pieces, as a socket does
    /// with a large request.
    struct ChunkedReader {
        data: Vec<u8>,
        position: usize,
        chunk: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.position == self.data.len() {
                return Err(io::Error::new(ErrorKind::WouldBlock, "would block"));
            }
            let end = (self.position + self.chunk).min(self.data.len());
            let piece = &self.data[self.position..end];
            let length = piece.len().min(buf.len());
            buf[..length].copy_from_slice(&piece[..length]);
            self.position += length;
            Ok(length)
        }
    }

    #[test]
    fn receive_request_that_arrives_in_pieces() {
        let content = format!(
            r#"{{ "seq": 3, "type": "request", "command": "test", "arguments": {{ "data": "{}" }} }}"#,
            "a".repeat(100_000)
        );
        let input = format!("Content-Length: {}\r\n\r\n{content}", content.len());

        let mut output = Vec::new();
        let mut adapter = DapAdapter::new(
            ChunkedReader {
                data: input.into_bytes(),
                position: 0,
                chunk: 64,
            },
            &mut output,
        );

        let request = adapter.listen_for_request().unwrap().unwrap();

        assert_eq!(request.command, "test");
    }

    #[test]
    fn request_summary_hides_file_payloads() {
        let request = Request {
            seq: 3,
            type_: "request".to_string(),
            command: "restart".to_string(),
            arguments: Some(serde_json::json!({
                "chipDescriptionData": "aaaa",
                "chip": "esp32c6",
                "coreConfigs": [{
                    "programBinary": "target/app",
                    "programBinaryData": "aaaaaaaa",
                }],
            })),
        };

        let summary = format!("{:?}", RequestSummary(&request));

        assert!(summary.contains(r#""chipDescriptionData": String("<4 bytes>")"#));
        assert!(summary.contains(r#""programBinaryData": String("<8 bytes>")"#));
        assert!(summary.contains(r#""chip": String("esp32c6")"#));
        assert!(summary.contains(r#""programBinary": String("target/app")"#));
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
