// Ignore clippy warning in the `schemafy!` output
#![allow(clippy::derive_partial_eq_without_eq)]

// use crate::dap_types2 as debugserver_types;
use crate::DebuggerError;
use num_traits::Num;
use parse_int::parse;
use probe_rs_cli_util::rtt;
use schemafy::schemafy;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;

// Convert the MSDAP `debugAdaptor.json` file into Rust types.
schemafy!(root: debugserver_types "src/debug_adapter/debugProtocol.json");

/// Custom 'quit' request, so that VSCode can tell the `probe-rs-debugger` to terminate its own process.
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct QuitRequest {
    /// Object containing arguments for the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<TerminateArguments>,
    /// The command to execute.
    pub command: String,
    /// Sequence number (also known as message ID). For protocol messages of type \'request\' this ID
    /// can be used to cancel the request.
    pub seq: i64,
    /// Message type.
    #[serde(rename = "type")]
    pub type_: String,
}

/// Custom [`RttWindowOpened`] request, so that VSCode can confirm once a specific RTT channel's window has opened.
/// `probe-rs-debugger` will delay polling RTT channels until the data window has opened. This ensure no RTT data is lost on the client.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct RttWindowOpened {
    /// Object containing arguments for the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<RttWindowOpenedArguments>,
    /// The command to execute.
    pub command: String,
    /// Sequence number (also known as message ID). For protocol messages of type `request` this ID
    /// can be used to cancel the request.
    pub seq: i64,
    /// Message type.
    #[serde(rename = "type")]
    pub type_: String,
}
///  Arguments for [`RttWindowOpened`] request.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttWindowOpenedArguments {
    /// The RTT channel number.
    pub channel_number: usize,
    pub window_is_open: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelEventBody {
    pub channel_number: usize,
    pub channel_name: String,
    pub data_format: rtt::DataFormat,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttDataEventBody {
    pub channel_number: usize,
    /// RTT output
    pub data: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all(serialize = "lowercase", deserialize = "PascalCase"))]
pub enum MessageSeverity {
    Information,
    Warning,
    Error,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct ShowMessageEventBody {
    pub severity: MessageSeverity,
    pub message: String,
}

impl TryFrom<&serde_json::Value> for ReadMemoryArguments {
    fn try_from(arguments: &serde_json::Value) -> Result<Self, Self::Error> {
        let count = get_int_argument(Some(arguments), "count", 1)?;
        let memory_reference = get_string_argument(Some(arguments), "memory_reference", 0)?;
        Ok(ReadMemoryArguments {
            count,
            memory_reference,
            offset: None,
        })
    }

    type Error = DebuggerError;
}

impl TryFrom<&serde_json::Value> for WriteMemoryArguments {
    type Error = DebuggerError;
    fn try_from(arguments: &serde_json::Value) -> Result<Self, Self::Error> {
        let memory_reference = get_string_argument(Some(arguments), "memory_reference", 0)?;
        let data = get_string_argument(Some(arguments), "data", 1)?;
        Ok(WriteMemoryArguments {
            data,
            memory_reference,
            offset: None,
            allow_partial: Some(false),
        })
    }
}

// SECTION: For various helper functions

/// Parse the argument at the given index.
///
/// Note: The function accepts an `Option` for `arguments` because this makes
/// the usage easier if no arguments are present.
pub fn get_int_argument<T: Num>(
    arguments: Option<&serde_json::Value>,
    argument_name: &str,
    index: usize,
) -> Result<T, DebuggerError>
where
    <T as Num>::FromStrRadixErr: std::error::Error,
    <T as Num>::FromStrRadixErr: Send,
    <T as Num>::FromStrRadixErr: Sync,
    <T as Num>::FromStrRadixErr: 'static,
{
    match arguments {
        Some(serde_json::Value::Array(arguments)) => {
            if arguments.len() <= index {
                return Err(DebuggerError::MissingArgument {
                    argument_name: argument_name.to_string(),
                });
            }
            if let Some(index_str) = arguments[index].as_str() {
                parse::<T>(index_str).map_err(|e| DebuggerError::ArgumentParseError {
                    argument_index: index,
                    argument: argument_name.to_string(),
                    source: e.into(),
                })
            } else {
                Err(DebuggerError::ArgumentParseError {
                    argument_index: index,
                    argument: argument_name.to_string(),
                    source: anyhow::anyhow!("Could not parse str at index: {}", index),
                })
            }
        }
        _ => Err(DebuggerError::MissingArgument {
            argument_name: argument_name.to_string(),
        }),
    }
}

fn get_string_argument(
    arguments: Option<&serde_json::Value>,
    argument_name: &str,
    index: usize,
) -> Result<String, DebuggerError> {
    match arguments {
        Some(serde_json::Value::Array(arguments)) => {
            if arguments.len() <= index {
                return Err(DebuggerError::MissingArgument {
                    argument_name: argument_name.to_string(),
                });
            }
            // Convert this to a Rust string.
            if let Some(index_str) = arguments[index].as_str() {
                Ok(index_str.to_string())
            } else {
                Err(DebuggerError::ArgumentParseError {
                    argument_index: index,
                    argument: argument_name.to_string(),
                    source: anyhow::anyhow!("Could not parse str at index: {}", index),
                })
            }
        }
        _ => Err(DebuggerError::MissingArgument {
            argument_name: argument_name.to_string(),
        }),
    }
}
