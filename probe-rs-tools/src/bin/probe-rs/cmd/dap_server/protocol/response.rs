use std::fmt::{self, Debug, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ErrorResponse, Request, Response};

#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ResponseKind {
    Ok(super::Response),
    Error(super::ErrorResponse),
}

impl ResponseKind {
    /// Creates a new successful response from a request ID and `Error` object.
    pub fn from_ok(request: &Request, body: Value) -> Self {
        ResponseKind::Ok(super::Response {
            body: Some(body),
            command: request.command.clone(),
            message: None,
            request_seq: request.seq,
            seq: 0,
            success: true,
            type_: "response".into(),
        })
    }

    /// Creates a new error response from a request ID and `Error` object.
    pub fn error_from_request(
        request: &Request,
        message: Option<String>,
        error: Option<super::Message>,
    ) -> Self {
        ResponseKind::Error(super::ErrorResponse {
            body: super::ErrorResponseBody { error },
            command: request.command.clone(),
            message,
            request_seq: request.seq,
            seq: 0,
            success: false,
            type_: "response".into(),
        })
    }

    /// Creates a new error response from a request ID and `Error` object.
    pub fn error_from_nothing(message: Option<String>, error: Option<super::Message>) -> Self {
        ResponseKind::Error(super::ErrorResponse {
            body: super::ErrorResponseBody { error },
            command: "".into(),
            message,
            request_seq: 0,
            seq: 0,
            success: false,
            type_: "response".into(),
        })
    }

    // /// Creates a new response from a request ID and either an `Ok(Value)` or `Err(Error)` body.
    // pub fn from_parts(id: Id, body: Result<Value>) -> Self {
    //     match body {
    //         Ok(result) => ResponseKind::from_ok(id, result),
    //         Err(error) => ResponseKind::from_error(id, error),
    //     }
    // }

    /// Splits the response into a request ID paired with either an `Ok(Value)` or `Err(Error)` to
    /// signify whether the response is a success or failure.
    pub fn into_parts(self) -> (i64, Result<super::Response, super::ErrorResponse>) {
        match self {
            ResponseKind::Ok(result) => (result.seq, Ok(result)),
            ResponseKind::Error(error) => (error.seq, Err(error)),
        }
    }

    /// Returns `true` if the response indicates success.
    pub const fn is_ok(&self) -> bool {
        matches!(self, ResponseKind::Ok(..))
    }

    /// Returns `true` if the response indicates failure.
    pub const fn is_error(&self) -> bool {
        !self.is_ok()
    }

    /// Returns the `result` value, if it exists.
    ///
    /// This member only exists if the response indicates success.
    pub const fn result(&self) -> Option<&super::Response> {
        match &self {
            ResponseKind::Ok(result) => Some(result),
            _ => None,
        }
    }

    /// Returns the `error` value, if it exists.
    ///
    /// This member only exists if the response indicates failure.
    pub const fn error(&self) -> Option<&super::ErrorResponse> {
        match &self {
            ResponseKind::Error(error) => Some(error),
            _ => None,
        }
    }

    /// Returns the corresponding request ID, if known.
    pub const fn set_seq(&mut self, seq: i64) {
        match self {
            ResponseKind::Ok(response) => response.seq = seq,
            ResponseKind::Error(error) => error.seq = seq,
        }
    }

    /// Returns the corresponding request ID, if known.
    pub const fn request_seq(&self) -> i64 {
        match self {
            ResponseKind::Ok(response) => response.request_seq,
            ResponseKind::Error(error) => error.request_seq,
        }
    }
}

impl FromStr for ResponseKind {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl From<Response> for ResponseKind {
    fn from(value: Response) -> Self {
        Self::Ok(value)
    }
}

impl From<ErrorResponse> for ResponseKind {
    fn from(value: ErrorResponse) -> Self {
        Self::Error(value)
    }
}
