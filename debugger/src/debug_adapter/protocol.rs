use crate::{
    debug_adapter::dap_types::{
        Event, MessageSeverity, OutputEventBody, ProtocolMessage, Request, Response,
        ShowMessageEventBody,
    },
    debugger::configuration::ConsoleLog,
    DebuggerError,
};
use anyhow::anyhow;
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    str,
    string::ToString,
};

pub trait ProtocolAdapter {
    /// Listen for a request. This call should be non-blocking, and if not request is available, it should
    /// return None.
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>>;

    fn send_event<S: Serialize>(
        &mut self,
        event_type: &str,
        event_body: Option<S>,
    ) -> anyhow::Result<()>;

    fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool;
    fn log_to_console<S: Into<String>>(&mut self, message: S) -> bool;
    fn set_console_log_level(&mut self, log_level: ConsoleLog);

    fn send_response<S: Serialize>(
        &mut self,
        request: Request,
        response: Result<Option<S>, DebuggerError>,
    ) -> anyhow::Result<()>;
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

    fn send_data(&mut self, raw_data: &[u8]) -> Result<(), std::io::Error> {
        let response_body = raw_data;

        let response_header = format!("Content-Length: {}\r\n\r\n", response_body.len());

        self.output.write_all(response_header.as_bytes())?;

        match self.output.write_all(response_body) {
            Ok(_) => {}
            Err(error) => {
                // TODO: Implement catch of errorkind "WouldBlock" and retry
                tracing::error!("send_data - body: {:?}", error);
                self.output.flush().ok();
                return Err(error);
            }
        }

        self.output.flush().ok();

        self.seq += 1;

        Ok(())
    }

    /// Receive data from `self.input`. Data has to be in the format specified by the Debug Adapter Protocol (DAP).
    /// The returned data is the content part of the request, as raw bytes.
    fn receive_data(&mut self) -> Result<Vec<u8>, DebuggerError> {
        let mut header = String::new();

        match self.input.read_line(&mut header) {
            Ok(_data_length) => {}
            Err(error) => {
                // There is no data available, so do something else (like checking the probe status) or try again.
                return Err(DebuggerError::NonBlockingReadError {
                    original_error: error,
                });
            }
        }

        // We should read an empty line here.
        let mut buff = String::new();
        match self.input.read_line(&mut buff) {
            Ok(_data_length) => {}
            Err(error) => {
                // There is no data available, so do something else (like checking the probe status) or try again.
                return Err(DebuggerError::NonBlockingReadError {
                    original_error: error,
                });
            }
        }

        let data_length = get_content_len(&header).ok_or_else(|| {
            DebuggerError::Other(anyhow!(
                "Failed to read content length from header '{}'",
                header
            ))
        })?;

        let mut content = vec![0u8; data_length];
        let bytes_read = match self.input.read(&mut content) {
            Ok(len) => len,
            Err(error) => {
                // There is no data available, so do something else (like checking the probe status) or try again.
                return Err(DebuggerError::NonBlockingReadError {
                    original_error: error,
                });
            }
        };

        if bytes_read == data_length {
            Ok(content)
        } else {
            Err(DebuggerError::Other(anyhow!(
                "Failed to read the expected {} bytes from incoming data",
                data_length
            )))
        }
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
                        self.log_to_console(format!("\nReceived DAP Request: {:#?}", request));
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
            Ok(message_content) => {
                // Extract protocol message
                match serde_json::from_slice::<ProtocolMessage>(&message_content) {
                    Ok(protocol_message) if protocol_message.type_ == "request" => {
                        match serde_json::from_slice::<Request>(&message_content) {
                            Ok(request) => Ok(Some(request)),
                            Err(error) => Err(DebuggerError::Other(anyhow!(
                                "Error encoding ProtocolMessage to Request: {:?}",
                                error
                            ))),
                        }
                    }
                    Ok(protocol_message) => Err(DebuggerError::Other(anyhow!(
                        "Received an unexpected message type: '{}'",
                        protocol_message.type_
                    ))),
                    Err(error) => Err(DebuggerError::Other(anyhow!("{}", error))),
                }
            }
            Err(error) => {
                match error {
                    DebuggerError::NonBlockingReadError { original_error } => {
                        if original_error.kind() == std::io::ErrorKind::WouldBlock {
                            // Non-blocking read is waiting for incoming data that is not ready yet.
                            // This is not a real error, so use this opportunity to check on probe status and notify the debug client if required.
                            Ok(None)
                        } else {
                            // This is a legitimate error. Tell the client about it.
                            Err(DebuggerError::StdIO(original_error))
                        }
                    }
                    _ => {
                        // This is a legitimate error. Tell the client about it.
                        Err(DebuggerError::Other(anyhow!("{}", error)))
                    }
                }
            }
        }
    }
}

impl<R: Read, W: Write> ProtocolAdapter for DapAdapter<R, W> {
    fn listen_for_request(&mut self) -> anyhow::Result<Option<Request>> {
        self.listen_for_request_and_respond()
    }

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

        self.send_data(&encoded_event)?;

        if new_event.event != "output" {
            // This would result in an endless loop.
            match self.console_log_level {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(format!("\nTriggered DAP Event: {}", new_event.event));
                }
                ConsoleLog::Debug => {
                    self.log_to_console(format!("INFO: Triggered DAP Event: {:#?}", new_event));
                }
            }
        }

        Ok(())
    }

    fn show_message(&mut self, severity: MessageSeverity, message: impl Into<String>) -> bool {
        tracing::debug!("show_message");

        let event_body = match serde_json::to_value(ShowMessageEventBody {
            severity,
            message: format!("{}\n", message.into()),
        }) {
            Ok(event_body) => event_body,
            Err(_) => {
                return false;
            }
        };
        self.send_event("probe-rs-show-message", Some(event_body))
            .is_ok()
    }

    fn log_to_console<S: Into<String>>(&mut self, message: S) -> bool {
        tracing::debug!("log_to_console");
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

    fn send_response<S: Serialize>(
        &mut self,
        request: Request,
        response: Result<Option<S>, DebuggerError>,
    ) -> anyhow::Result<()> {
        let mut resp = Response {
            command: request.command.clone(),
            request_seq: request.seq,
            seq: request.seq,
            success: false,
            body: None,
            type_: "response".to_owned(),
            message: None,
        };

        match response {
            Ok(value) => {
                let body_value = match value {
                    Some(value) => Some(serde_json::to_value(value)?),
                    None => None,
                };
                resp.success = true;
                resp.body = body_value;
            }
            Err(debugger_error) => {
                resp.success = false;
                resp.message = {
                    let mut response_message = debugger_error.to_string();
                    let mut offset_iterations = 0;
                    let mut child_error: Option<&dyn std::error::Error> =
                        std::error::Error::source(&debugger_error);
                    while let Some(source_error) = child_error {
                        offset_iterations += 1;
                        response_message = format!("{}\n", response_message,);
                        for _offset_counter in 0..offset_iterations {
                            response_message = format!("{}\t", response_message);
                        }
                        response_message = format!(
                            "{}{:?}",
                            response_message,
                            <dyn std::error::Error>::to_string(source_error)
                        );
                        child_error = std::error::Error::source(source_error);
                    }
                    Some(response_message)
                };
            }
        };

        tracing::debug!("send_response: {:?}", resp);

        // Check if we got a request for this response
        if let Some(request_command) = self.pending_requests.remove(&resp.request_seq) {
            assert_eq!(request_command, resp.command);
        } else {
            tracing::error!("Trying to send a response to non-existing request! Response {:?} has no pending request", resp);
        }

        let encoded_resp = serde_json::to_vec(&resp)?;

        self.send_data(&encoded_resp)?;

        if !resp.success {
            self.log_to_console(
                &resp
                    .clone()
                    .message
                    .unwrap_or_else(|| "<empty message>".to_string()),
            );
            self.show_message(
                MessageSeverity::Error,
                &resp
                    .message
                    .unwrap_or_else(|| "<empty message>".to_string()),
            );
        } else {
            match self.console_log_level {
                ConsoleLog::Console => {}
                ConsoleLog::Info => {
                    self.log_to_console(format!(
                        "   Sent DAP Response sequence #{} : {}",
                        resp.seq, resp.command
                    ));
                }
                ConsoleLog::Debug => {
                    self.log_to_console(format!("\nSent DAP Response: {:#?}", resp));
                }
            }
        }

        Ok(())
    }

    fn set_console_log_level(&mut self, log_level: ConsoleLog) {
        self.console_log_level = log_level;
    }
}

fn get_content_len(header: &str) -> Option<usize> {
    let mut parts = header.trim_end().split_ascii_whitespace();

    // discard first part
    let first_part = parts.next()?;

    if first_part == "Content-Length:" {
        parts.next()?.parse::<usize>().ok()
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use std::io::{self, ErrorKind, Read};

    use crate::debug_adapter::protocol::{get_content_len, ProtocolAdapter};

    use super::DapAdapter;

    struct TestReader {
        response: Option<io::Result<usize>>,
    }

    impl Read for TestReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            if let Some(response) = self.response.take() {
                response
            } else {
                Err(io::Error::new(
                    ErrorKind::Other,
                    "Repeated use of test reader",
                ))
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
    fn receive_request_with_wrong_content_length() {
        let content = "{ \"seq\": 3, \"type\": \"request\", \"command\": \"test\" }";

        let input = format!("Content-Length: {}\r\n\r\n{}", content.len() + 10, content);

        let mut output = Vec::new();

        let mut adapter = DapAdapter::new(input.as_bytes(), &mut output);
        adapter.console_log_level = super::ConsoleLog::Info;

        let _request = adapter.listen_for_request().unwrap_err();

        let output_str = String::from_utf8(output).unwrap();

        insta::assert_snapshot!(output_str);
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

    #[test]
    fn parse_valid_header() {
        let header = "Content-Length: 234\r\n";

        assert_eq!(234, get_content_len(header).unwrap());
    }

    #[test]
    fn parse_invalid_header() {
        let header = "Content: 234\r\n";

        assert!(get_content_len(header).is_none());
    }
}
