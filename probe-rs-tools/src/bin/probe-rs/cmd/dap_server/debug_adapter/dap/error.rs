use std::collections::BTreeMap;

use crate::cmd::dap_server::DebuggerError;

use super::dap_types::{ErrorResponseBody, Message, Request, Response};

pub trait ErrorResponseExt {
    fn from_error(request: Request, seq: i64, error: DebuggerError) -> Response;
}

impl ErrorResponseExt for Response {
    fn from_error(request: Request, seq: i64, error: DebuggerError) -> Response {
        let response_message = error.to_string();
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
            seq,
            success: false,
            type_: "response".to_owned(),
            message: Some("cancelled".to_string()), // Predefined value in the MSDAP spec.
            body: Some(serde_json::to_value(response_body).unwrap()),
        }
    }
}
