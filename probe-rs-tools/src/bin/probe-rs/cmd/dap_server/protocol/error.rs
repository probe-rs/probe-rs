//! Error types defined by the JSON-RPC specification.

use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A specialized [`Result`] error type for JSON-RPC handlers.
///
/// [`Result`]: enum@std::result::Result
pub type Result<T> = std::result::Result<T, Error>;

/// A list of numeric error codes used in JSON-RPC responses.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(into = "i64", from = "i64")]
pub enum ErrorCode {
    /// Invalid JSON was received by the server.
    ParseError,
    /// The JSON sent is not a valid Request object.
    InvalidRequest,
    /// The method does not exist / is not available.
    MethodNotFound,
    /// Invalid method parameter(s).
    InvalidParams,
    /// Internal JSON-RPC error.
    InternalError,
    /// Reserved for implementation-defined server errors.
    ServerError(i64),

    /// The request was cancelled by the client.
    ///
    /// # Compatibility
    ///
    /// This error code is defined by the Language Server Protocol.
    RequestCancelled,
    /// The request was invalidated by another incoming request.
    ///
    /// # Compatibility
    ///
    /// This error code is specific to the Language Server Protocol.
    ContentModified,
}

impl ErrorCode {
    /// Returns the integer error code value.
    pub const fn code(&self) -> i64 {
        match *self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::RequestCancelled => -32800,
            ErrorCode::ContentModified => -32801,
            ErrorCode::ServerError(code) => code,
        }
    }

    /// Returns a human-readable description of the error.
    pub const fn description(&self) -> &'static str {
        match *self {
            ErrorCode::ParseError => "Parse error",
            ErrorCode::InvalidRequest => "Invalid request",
            ErrorCode::MethodNotFound => "Method not found",
            ErrorCode::InvalidParams => "Invalid params",
            ErrorCode::InternalError => "Internal error",
            ErrorCode::RequestCancelled => "Canceled",
            ErrorCode::ContentModified => "Content modified",
            ErrorCode::ServerError(_) => "Server error",
        }
    }
}

impl From<i64> for ErrorCode {
    fn from(code: i64) -> Self {
        match code {
            -32700 => ErrorCode::ParseError,
            -32600 => ErrorCode::InvalidRequest,
            -32601 => ErrorCode::MethodNotFound,
            -32602 => ErrorCode::InvalidParams,
            -32603 => ErrorCode::InternalError,
            -32800 => ErrorCode::RequestCancelled,
            -32801 => ErrorCode::ContentModified,
            code => ErrorCode::ServerError(code),
        }
    }
}

impl From<ErrorCode> for i64 {
    fn from(code: ErrorCode) -> Self {
        code.code()
    }
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&self.code(), f)
    }
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    /// A number indicating the error type that occurred.
    pub code: ErrorCode,
    /// A short description of the error.
    pub message: Cow<'static, str>,
    /// Additional information about the error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    /// Creates a new error from the given `ErrorCode`.
    pub const fn new(code: ErrorCode) -> Self {
        Error {
            code,
            message: Cow::Borrowed(code.description()),
            data: None,
        }
    }

    /// Creates a new parse error (`-32700`).
    pub const fn parse_error() -> Self {
        Error::new(ErrorCode::ParseError)
    }

    /// Creates a new "invalid request" error (`-32600`).
    pub const fn invalid_request() -> Self {
        Error::new(ErrorCode::InvalidRequest)
    }

    /// Creates a new "method not found" error (`-32601`).
    pub const fn method_not_found() -> Self {
        Error::new(ErrorCode::MethodNotFound)
    }

    /// Creates a new "invalid params" error (`-32602`).
    pub fn invalid_params<M>(message: M) -> Self
    where
        M: Into<Cow<'static, str>>,
    {
        Error {
            code: ErrorCode::InvalidParams,
            message: message.into(),
            data: None,
        }
    }

    /// Creates a new internal error (`-32603`).
    pub const fn internal_error() -> Self {
        Error::new(ErrorCode::InternalError)
    }

    /// Creates a new "request cancelled" error (`-32800`).
    ///
    /// # Compatibility
    ///
    /// This error code is defined by the Language Server Protocol.
    pub const fn request_cancelled() -> Self {
        Error::new(ErrorCode::RequestCancelled)
    }

    /// Creates a new "content modified" error (`-32801`).
    ///
    /// # Compatibility
    ///
    /// This error code is defined by the Language Server Protocol.
    pub const fn content_modified() -> Self {
        Error::new(ErrorCode::ContentModified)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.code.description(), self.message)
    }
}

impl std::error::Error for Error {}

/// Error response returned for every request received before the server is initialized.
///
/// See [here](https://microsoft.github.io/language-server-protocol/specification#initialize)
/// for reference.
pub(crate) const fn not_initialized_error() -> Error {
    Error {
        code: ErrorCode::ServerError(-32002),
        message: Cow::Borrowed("Server not initialized"),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_serializes_as_i64() {
        let serialized = serde_json::to_string(&ErrorCode::ParseError).unwrap();
        assert_eq!(serialized, "-32700");

        let serialized = serde_json::to_string(&ErrorCode::ServerError(-12345)).unwrap();
        assert_eq!(serialized, "-12345");
    }

    #[test]
    fn error_code_deserializes_from_i64() {
        let deserialized: ErrorCode = serde_json::from_str("-32700").unwrap();
        assert_eq!(deserialized, ErrorCode::ParseError);

        let deserialized: ErrorCode = serde_json::from_str("-12345").unwrap();
        assert_eq!(deserialized, ErrorCode::ServerError(-12345));
    }
}
