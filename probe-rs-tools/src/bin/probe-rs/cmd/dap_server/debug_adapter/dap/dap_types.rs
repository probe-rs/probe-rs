// use crate::dap_types2 as debugserver_types;
use crate::cmd::dap_server::DebuggerError;
use crate::util::rtt;
use num_traits::Num;
use parse_int::parse;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

// Convert the MSDAP `debugAdaptor.json` file into Rust types.
schemafy::schemafy!(root: debugserver_types "src/bin/probe-rs/cmd/dap_server/debug_adapter/dap/debugProtocol.json");

/// Memory addresses come in as strings, but we want to use them as u64s.
pub struct MemoryAddress(pub u64);

impl TryFrom<&str> for MemoryAddress {
    type Error = DebuggerError;
    /// Convert either a decimal or hexadecimal string into a `MemoryAddress(u64)`.
    fn try_from(string_address: &str) -> Result<Self, Self::Error> {
        Ok(MemoryAddress(
            if string_address[..2].eq_ignore_ascii_case("0x") {
                u64::from_str_radix(&string_address[2..], 16)
            } else {
                string_address.parse()
            }
            .map_err(|error| {
                DebuggerError::UserMessage(format!(
                    "Invalid memory address: {string_address:?}: {error:?}"
                ))
            })?,
        ))
    }
}

/// Arguments for custom [`RttWindowOpened`] request, so that VSCode can confirm once a specific RTT channel's window has opened.
/// `probe-rs-debugger` will delay polling RTT channels until the data window has opened. This ensure no RTT data is lost on the client.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttWindowOpenedArguments {
    /// The RTT channel number.
    pub channel_number: u32,
    pub window_is_open: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttChannelEventBody {
    pub channel_number: u32,
    pub channel_name: String,
    pub data_format: rtt::DataFormat,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RttDataEventBody {
    pub channel_number: u32,
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

impl Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name.as_deref().unwrap_or(""))
    }
}

impl Display for DisassembledInstruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{} : [{:<12}] {:<40}  {}",
            self.address,
            self.instruction_bytes.as_deref().unwrap_or(""),
            self.instruction,
            if let (Some(file), Some(line), Some(column)) = (
                self.location.as_ref().map(|s| s.to_string()),
                self.line,
                self.column
            ) {
                format!("<{file}:{line}:{column}>")
            } else {
                "".to_string()
            },
        )?;
        Ok(())
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
                    source: anyhow::anyhow!("Could not parse str at index: {index}"),
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
                    source: anyhow::anyhow!("Could not parse str at index: {index}"),
                })
            }
        }
        _ => Err(DebuggerError::MissingArgument {
            argument_name: argument_name.to_string(),
        }),
    }
}

impl Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut formatted = self.format.clone();

        if let Some(args) = &self.variables {
            for (key, value) in args {
                formatted = formatted.replace(&format!("{{{key}}}"), value);
            }
        }

        f.write_str(&formatted)
    }
}
