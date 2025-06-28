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
    sync::{
        atomic::AtomicI64,
        mpsc::{Receiver, Sender},
    },
    thread,
};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, Encoder},
};
use tracing::instrument;

use super::codec::{DapCodec, Frame, Message};

pub trait EventSender {
    // Send an event
    //
    // This might fail if the connection to the client has been lost.
    fn send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()>;
}

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>;

    fn event_sender(&self) -> Box<dyn EventSender>;

    fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()>;

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String>;

    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn console_log_level(&self) -> ConsoleLog;

    /// Increases the sequence number by 1 and returns it.
    fn get_next_seq(&mut self) -> i64;
}

pub trait ProtocolHelper {
    fn show_message(&mut self, severity: MessageSeverity, message: impl AsRef<str>) -> bool;

    /// Log a message to the console. Returns false if logging the message failed.
    fn log_to_console(&mut self, message: impl AsRef<str>) -> bool;

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
    fn show_message(&mut self, severity: MessageSeverity, message: impl AsRef<str>) -> bool {
        let msg = message.as_ref();

        tracing::debug!("show_message: {msg}");

        let event_body = match serde_json::to_value(ShowMessageEventBody {
            severity,
            message: format!("{msg}\n"),
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.send_event("probe-rs-show-message", Some(event_body))
            .is_ok()
    }

    fn log_to_console(&mut self, message: impl AsRef<str>) -> bool {
        let event_body = match serde_json::to_value(OutputEventBody {
            output: format!("{}\n", message.as_ref()),
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

enum TxMessage {
    Frame(Frame<Message>),
    Done,
}

pub struct DapAdapter<R> {
    input: BufReader<R>,

    output_tx: Sender<TxMessage>,
    output_thread: Option<std::thread::JoinHandle<Result<(), std::io::Error>>>,

    console_log_level: ConsoleLog,

    pending_requests: HashMap<i64, String>,

    codec: DapCodec<Message>,
    input_buffer: BytesMut,
}

impl<R> Drop for DapAdapter<R> {
    fn drop(&mut self) {
        // Signal to the output thread that we're done
        let _ = self.output_tx.send(TxMessage::Done);

        if let Some(output_thread) = self.output_thread.take() {
            tracing::debug!("Waiting for TX thread to join");
            let _ = output_thread.join();
        }
    }
}

fn output_thread<W: Write>(mut write: W, rx: Receiver<TxMessage>) -> Result<(), std::io::Error> {
    let mut buf = BytesMut::with_capacity(4096);

    for msg in rx {
        match msg {
            TxMessage::Done => break,
            TxMessage::Frame(frame) => {
                DapCodec::new().encode(frame, &mut buf)?;
                write.write_all(&buf)?;
                write.flush()?;

                buf.clear();
            }
        }
    }

    Ok(())
}

impl<R: Read> DapAdapter<R> {
    /// Create a new DAP adapter.
    ///
    /// Can fail if the output thread fails to start.
    pub(crate) fn new<W: Write + Send + 'static>(
        reader: R,
        writer: W,
    ) -> Result<Self, std::io::Error> {
        let (tx, rx) = std::sync::mpsc::channel();

        let output_thread = thread::Builder::new()
            .name("dap_tx".to_string())
            .spawn(move || output_thread(writer, rx))?;

        Ok(Self {
            input: BufReader::new(reader),
            output_tx: tx,
            output_thread: Some(output_thread),
            console_log_level: ConsoleLog::Console,
            pending_requests: HashMap::new(),

            codec: DapCodec::new(),
            input_buffer: BytesMut::with_capacity(4096),
        })
    }
}

impl<R> DapAdapter<R> {
    #[instrument(level = "trace", skip_all)]
    fn send_data(&mut self, item: Frame<Message>) -> Result<(), std::io::Error> {
        // TODO: Better error handling
        //
        // The only case where this would fail is if the connection to the client is closed.

        self.output_tx
            .send(TxMessage::Frame(item))
            .map_err(|e| std::io::Error::other(e))
    }
}

impl<R: Read> DapAdapter<R> {
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

impl<R: Read> ProtocolAdapter for DapAdapter<R> {
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
                    self.log_to_console(format!("\nTriggered DAP Event: {event_type}"));
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
        get_next_seq()
    }

    fn event_sender(&self) -> Box<dyn EventSender> {
        Box::new(EventSenderThingy::new(self.output_tx.clone()))
    }
}

static SEQ: AtomicI64 = AtomicI64::new(1);

fn get_next_seq() -> i64 {
    // Ordering: We use Relaxed ordering because we don't care about the order of the sequence numbers,
    // only that they are unique.
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct EventSenderThingy {
    tx: Sender<TxMessage>,
}

impl EventSenderThingy {
    fn new(tx: Sender<TxMessage>) -> Self {
        Self { tx }
    }
}

impl EventSender for EventSenderThingy {
    fn send_event(
        &mut self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let new_event = Event {
            seq: get_next_seq(),
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body,
        };

        self.tx
            .send(TxMessage::Frame(Frame::new(Message::Event(new_event))))
            .context("Failed to send event")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use std::{
        io::{self, ErrorKind},
        sync::{Arc, Mutex},
    };

    use super::*;

    struct TestReader {
        response: Option<io::Result<usize>>,
    }

    #[derive(Clone)]
    struct TestOutput {
        data: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for TestOutput {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.data.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.data.lock().unwrap().flush()
        }
    }

    impl TestOutput {
        fn new() -> Self {
            Self {
                data: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn data(&self) -> Vec<u8> {
            self.data.lock().unwrap().clone()
        }
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

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().unwrap().unwrap();

        // Ensure that all the output is sent
        drop(adapter);

        let output_str = String::from_utf8(output.data()).unwrap();

        insta::assert_snapshot!(output_str);

        assert_eq!(request.command, "test");
        assert_eq!(request.seq, 3);
    }

    #[test]
    fn receive_request_with_invalid_json() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let _request = adapter.listen_for_request().unwrap_err();

        // Ensure that all the output is sent
        drop(adapter);

        let output_str = String::from_utf8(output.data()).unwrap();

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

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input, output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().unwrap();

        let output_str = String::from_utf8(output.data()).unwrap();

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
        let mut adapter = DapAdapter::new(io::empty(), FailingWriter {}).unwrap();

        let result = adapter.send_event("probe-rs-test", Some(()));

        // TODO: This now can't fail, because the actual send is done in a different thread
        //
        // Look into error reporting for that case
        assert!(result.is_ok());
    }

    #[test]
    fn message_send_error() {
        let mut adapter = DapAdapter::new(io::empty(), FailingWriter {}).unwrap();

        let result = adapter.show_message(MessageSeverity::Error, "probe-rs-test");

        // TODO: This now can't fail, because the actual send is done in a different thread
        //
        // Look into error reporting for that case
        assert!(result);
    }
}
