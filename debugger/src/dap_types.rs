// use crate::dap_types2 as debugserver_types;
use crate::DebuggerError;
use num_traits::Num;
use parse_int::parse;
use schemafy::schemafy;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;

schemafy!(root: debugserver_types "src/debugProtocol.json");

#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct RttEventBody {
    pub channel: usize,
    pub format: crate::rtt::channel::DataFormat,
    #[doc = " RTT output"]
    pub data: String,
}
#[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
pub struct ShowMessageEventBody {
    /// The `severity` field can be one of "information", "warning", or "error"
    pub severity: String,
    pub message: String,
}

impl TryFrom<&serde_json::Value> for ReadMemoryArguments {
    fn try_from(arguments: &serde_json::Value) -> Result<Self, Self::Error> {
        let count = get_int_argument(arguments, "count", 1)?;
        let memory_reference = get_string_argument(arguments, "memory_reference", 0)?;
        Ok(ReadMemoryArguments {
            count,
            memory_reference,
            offset: None,
        })
    }

    type Error = DebuggerError;
}

// SECTION: For various helper functions

/// Parse the argument at the given index.
pub fn get_int_argument<T: Num>(
    arguments: &serde_json::Value,
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
        serde_json::Value::Array(arguments) => {
            if arguments.len() <= index {
                return Err(DebuggerError::MissingArgument {
                    argument_name: argument_name.to_string(),
                });
            }
            parse::<T>(arguments[index].as_str().unwrap()).map_err(|e| {
                DebuggerError::ArgumentParseError {
                    argument_index: index,
                    argument: argument_name.to_string(),
                    source: e.into(),
                }
            })
        }
        _ => Err(DebuggerError::MissingArgument {
            argument_name: argument_name.to_string(),
        }),
    }
}

fn get_string_argument(
    arguments: &serde_json::Value,
    argument_name: &str,
    index: usize,
) -> Result<String, DebuggerError> {
    match arguments {
        serde_json::Value::Array(arguments) => {
            if arguments.len() <= index {
                return Err(DebuggerError::MissingArgument {
                    argument_name: argument_name.to_string(),
                });
            }
            Ok(arguments[index].as_str().unwrap().to_string()) //convert this to RUST string
        }
        _ => Err(DebuggerError::MissingArgument {
            argument_name: argument_name.to_string(),
        }),
    }
}
