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
    io::ErrorKind,
    pin::Pin,
    str,
    sync::atomic::AtomicI64,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::mpsc::{UnboundedReceiver, UnboundedSender, WeakUnboundedSender},
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
        &self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()>;
}

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    async fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>;

    fn event_sender(&self) -> Box<dyn EventSender>;

    async fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()>;

    fn remove_pending_request(&mut self, request_seq: i64) -> Option<String>;

    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn console_log_level(&self) -> ConsoleLog;
}

pub trait ProtocolHelper {
    fn show_message(&mut self, severity: MessageSeverity, message: impl AsRef<str>) -> bool;

    /// Log a message to the console. Returns false if logging the message failed.
    fn log_to_console(&mut self, message: impl AsRef<str>) -> bool;

    async fn send_response<S: Serialize + std::fmt::Debug>(
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

    async fn send_response<S: Serialize + std::fmt::Debug>(
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
                seq: 0, // This will get filled when it's sent
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
                    seq: 0, // This will get filled when it's sent
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
            .await
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

#[derive(Debug)]
enum TxMessage {
    Frame(Message),
    Done,
}

pub struct DapAdapter<R> {
    input: Pin<Box<BufReader<R>>>,
    codec: DapCodec<Message>,
    input_buffer: BytesMut,

    output_tx: UnboundedSender<TxMessage>,
    output_task: Option<tokio::task::JoinHandle<Result<(), std::io::Error>>>,

    console_log_level: ConsoleLog,

    pending_requests: HashMap<i64, String>,
}

impl<R> Drop for DapAdapter<R> {
    fn drop(&mut self) {
        // Signal to the output thread that we're done
        let _ = self.output_tx.send(TxMessage::Done);

        if let Some(output_thread) = self.output_task.take() {
            // TODO: Do we actually need to abort here?

            tracing::debug!("Waiting for TX thread to join");
            output_thread.abort();
        }
    }
}

async fn output_task<W: AsyncWrite>(
    write: W,
    mut rx: UnboundedReceiver<TxMessage>,
) -> Result<(), std::io::Error> {
    let mut writer = Box::pin(write);

    let mut buf = BytesMut::with_capacity(4096);

    let mut seq = 1;

    while let Some(msg) = rx.recv().await {
        match msg {
            TxMessage::Done => break,
            TxMessage::Frame(mut msg) => {
                msg.set_seq(seq);
                seq += 1;

                let frame = Frame::new(msg);

                DapCodec::new().encode(frame, &mut buf)?;
                writer.write_all(&buf).await?;
                writer.flush().await?;

                buf.clear();
            }
        }
    }

    Ok(())
}

impl<R: AsyncRead> DapAdapter<R> {
    /// Create a new DAP adapter.
    ///
    /// Can fail if the output thread fails to start.
    pub(crate) fn new<W: AsyncWrite + Send + 'static>(
        reader: R,
        writer: W,
    ) -> Result<Self, std::io::Error> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let output_thread = tokio::spawn(output_task(writer, rx));

        Ok(Self {
            input: Box::pin(BufReader::new(reader)),
            output_tx: tx,
            output_task: Some(output_thread),
            console_log_level: ConsoleLog::Console,
            pending_requests: HashMap::new(),

            codec: DapCodec::new(),
            input_buffer: BytesMut::with_capacity(4096),
        })
    }

    /// This is prefered to drop so that it is ensured that all data is sent
    ///
    /// Returns an error if the output thread fails to join,
    /// of if there was an error sending all pending messages.
    #[cfg(test)]
    pub async fn close(mut self) -> Result<(), std::io::Error> {
        // Signal to the output thread that we're done
        let _ = self.output_tx.send(TxMessage::Done);

        if let Some(output_thread) = self.output_task.take() {
            output_thread.await?
        } else {
            Ok(())
        }
    }
}

impl<R> DapAdapter<R> {
    #[instrument(level = "trace", skip_all)]
    fn send_data(&mut self, item: Message) -> Result<(), std::io::Error> {
        // TODO: Better error handling
        //
        // The only case where this would fail is if the connection to the client is closed.

        self.output_tx
            .send(TxMessage::Frame(item))
            .map_err(std::io::Error::other)
    }
}

impl<R: AsyncRead> DapAdapter<R> {
    /// Receive data from `self.input`. Data has to be in the format specified by the Debug Adapter Protocol (DAP).
    /// The returned data is the content part of the request, as raw bytes.
    async fn receive_data(&mut self) -> Result<Option<Frame<Message>>, DebuggerError> {
        match self.input.fill_buf().await {
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

    async fn listen_for_request_and_respond(&mut self) -> anyhow::Result<Option<Request>> {
        match self.receive_msg_content().await {
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

    async fn receive_msg_content(&mut self) -> Result<Option<Request>, DebuggerError> {
        match self.receive_data().await {
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

impl<R: AsyncRead> ProtocolAdapter for DapAdapter<R> {
    async fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.listen_for_request_and_respond().await
    }

    #[instrument(level = "trace", skip_all)]
    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()> {
        let new_event = Event {
            seq: 0, // This will get filled when it's sent
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body.map(|event_body| serde_json::to_value(event_body).unwrap_or_default()),
        };

        let result = self
            .send_data(new_event.clone().into())
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

    async fn send_raw_response(&mut self, response: Response) -> anyhow::Result<()> {
        self.send_data(Message::Response(response))?;

        Ok(())
    }

    fn event_sender(&self) -> Box<dyn EventSender> {
        Box::new(EventSenderThingy::new(self.output_tx.downgrade()))
    }
}

static SEQ: AtomicI64 = AtomicI64::new(1);

fn get_next_seq() -> i64 {
    // Ordering: We use Relaxed ordering because we don't care about the order of the sequence numbers,
    // only that they are unique.
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

struct EventSenderThingy {
    tx: WeakUnboundedSender<TxMessage>,
}

impl EventSenderThingy {
    fn new(tx: WeakUnboundedSender<TxMessage>) -> Self {
        Self { tx }
    }
}

impl EventSender for EventSenderThingy {
    fn send_event(
        &self,
        event_type: &str,
        event_body: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let new_event = Event {
            seq: get_next_seq(),
            type_: "event".to_string(),
            event: event_type.to_string(),
            body: event_body,
        };

        let sender = self
            .tx
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Unable to send event, connection to client dropped"))?;

        sender
            .send(TxMessage::Frame(Message::Event(new_event)))
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
        response: Option<io::Result<Vec<u8>>>,
    }

    #[derive(Clone)]
    struct TestOutput {
        data: Arc<Mutex<Vec<u8>>>,
    }

    impl AsyncWrite for TestOutput {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, io::Error>> {
            self.data.lock().unwrap().extend_from_slice(buf);

            let len = buf.len();
            std::task::Poll::Ready(Ok(len))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            let buf: &mut Vec<u8> = &mut self.data.lock().unwrap();
            let result = std::io::Write::flush(buf);

            std::task::Poll::Ready(result)
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Ok(()))
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

    impl AsyncRead for TestReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if let Some(response) = self.response.take() {
                match response {
                    Ok(data) => {
                        buf.put_slice(&data);
                        std::task::Poll::Ready(Ok(()))
                    }
                    Err(err) => std::task::Poll::Ready(Err(err)),
                }
            } else {
                std::task::Poll::Ready(Err(io::Error::other("Repeated use of test reader")))
            }
        }
    }

    #[tokio::test]
    async fn receive_valid_request() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test\" }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().await.unwrap().unwrap();

        // Ensure that all the output is sent
        adapter.close().await.unwrap();

        let output_str = String::from_utf8(output.data()).unwrap();

        insta::assert_snapshot!(output_str);

        assert_eq!(request.command, "test");
        assert_eq!(request.seq, 3);
    }

    #[tokio::test]
    async fn receive_request_with_invalid_json() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len(), content);

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let _request = adapter.listen_for_request().await.unwrap_err();

        // Ensure that all the output is sent
        adapter.close().await.unwrap();

        let output_str = String::from_utf8(output.data()).unwrap();

        insta::assert_snapshot!(output_str);
    }

    #[tokio::test]
    async fn receive_request_would_block() {
        let input = TestReader {
            response: Some(io::Result::Err(io::Error::new(
                ErrorKind::WouldBlock,
                "would block",
            ))),
        };

        let output = TestOutput::new();

        let mut adapter = DapAdapter::new(input, output.clone()).unwrap();
        adapter.console_log_level = super::ConsoleLog::Info;

        let request = adapter.listen_for_request().await.unwrap();

        let output_str = String::from_utf8(output.data()).unwrap();

        insta::assert_snapshot!(output_str);

        assert!(request.is_none());
    }

    struct FailingWriter {}

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<Result<usize, io::Error>> {
            std::task::Poll::Ready(Err(io::Error::other("FailingWriter")))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Err(io::Error::other("FailingWriter")))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), io::Error>> {
            std::task::Poll::Ready(Err(io::Error::other("FailingWriter")))
        }
    }

    #[tokio::test]
    async fn event_send_error() {
        let mut adapter = DapAdapter::new(tokio::io::empty(), FailingWriter {}).unwrap();

        let result = adapter.send_event("probe-rs-test", Some(()));

        // This should fail because we get the result from the failing write here,
        // which is reported back from the task
        adapter.close().await.unwrap_err();

        // TODO: This now can't fail, because the actual send is done in a different thread
        //
        // Look into error reporting for that case
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn message_send_error() {
        let mut adapter = DapAdapter::new(tokio::io::empty(), FailingWriter {}).unwrap();

        let result = adapter.show_message(MessageSeverity::Error, "probe-rs-test");

        // This should fail because we get the result from the failing write here,
        // which is reported back from the task
        adapter.close().await.unwrap_err();

        // TODO: This now can't fail, because the actual send is done in a different thread
        //
        // Look into error reporting for that case
        assert!(result);
    }
}
