use crate::cmd::dap_server::DebuggerError;
use std::{fmt::Display, str::FromStr};

pub(crate) enum ReplCommandArgs {
    Required(&'static str),
    Optional(&'static str),
}

impl Display for ReplCommandArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplCommandArgs::Required(arg_value) => {
                write!(f, "{arg_value}")
            }
            ReplCommandArgs::Optional(arg_value) => {
                write!(f, "[{arg_value}]")
            }
        }
    }
}

/// Limited subset of gdb format specifiers
#[derive(PartialEq)]
pub(crate) enum GdbFormat {
    /// Same as GDB's `t` format specifier
    Binary,
    /// Same as GDB's `x` format specifier
    Hex,
    /// Same as GDB's `i` format specifier
    Instruction,
    /// DAP variable reference, will show up in the REPL as a clickable link.
    DapReference,
    /// Native (as defined by data type) format.
    Native,
}

impl TryFrom<&char> for GdbFormat {
    type Error = DebuggerError;

    fn try_from(format: &char) -> Result<Self, Self::Error> {
        match format {
            't' => Ok(GdbFormat::Binary),
            'x' => Ok(GdbFormat::Hex),
            'i' => Ok(GdbFormat::Instruction),
            'v' => Ok(GdbFormat::DapReference),
            'n' => Ok(GdbFormat::Native),
            _ => Err(DebuggerError::UserMessage(format!(
                "Invalid format specifier: {format}"
            ))),
        }
    }
}

impl Display for GdbFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GdbFormat::Binary => write!(f, "t(binary)"),
            GdbFormat::Hex => write!(f, "x(hexadecimal)"),
            GdbFormat::Instruction => write!(f, "i(nstruction)"),
            GdbFormat::DapReference => write!(f, "v(ariable structured for DAP/VSCode)"),
            GdbFormat::Native => write!(f, "n(ative - as defined by data type)"),
        }
    }
}

pub(crate) enum GdbUnit {
    Byte,
    HalfWord,
    Word,
    Giant,
}

impl TryFrom<&char> for GdbUnit {
    type Error = DebuggerError;

    fn try_from(unit_size: &char) -> Result<Self, Self::Error> {
        match unit_size {
            'b' => Ok(GdbUnit::Byte),
            'h' => Ok(GdbUnit::HalfWord),
            'w' => Ok(GdbUnit::Word),
            'g' => Ok(GdbUnit::Giant),
            _ => Err(DebuggerError::UserMessage(format!(
                "Invalid unit size: {unit_size}"
            ))),
        }
    }
}

impl GdbUnit {
    fn get_size(&self) -> usize {
        match self {
            GdbUnit::Byte => 1,
            GdbUnit::HalfWord => 2,
            GdbUnit::Word => 4,
            GdbUnit::Giant => 8,
        }
    }
}

impl Display for GdbUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GdbUnit::Byte => write!(f, "b(yte)"),
            GdbUnit::HalfWord => write!(f, "h(alfword)"),
            GdbUnit::Word => write!(f, "w(ord)"),
            GdbUnit::Giant => write!(f, "g(iant)"),
        }
    }
}

/// The term 'Nuf' is borrowed from gdb's `x` command arguments, and stands for N(umber or count of units), U(nit size), and F(ormat specifier).
pub(crate) struct GdbNuf {
    pub(crate) unit_count: usize,
    pub(crate) unit_specifier: GdbUnit,
    pub(crate) format_specifier: GdbFormat,
}

impl GdbNuf {
    // TODO: If the format_specifier is `instruction` we should return the size of the instruction for the architecture.
    pub(crate) fn get_size(&self) -> usize {
        self.unit_count * self.unit_specifier.get_size()
    }
    // Validate that the format specifier is valid for a given range of supported formats
    pub(crate) fn check_supported_formats(
        &self,
        supported_list: &[GdbFormat],
    ) -> Result<(), String> {
        if supported_list.contains(&self.format_specifier) {
            Ok(())
        } else {
            let mut message = if supported_list.is_empty() {
                "No supported formats for this command.".to_string()
            } else {
                "".to_string()
            };
            for supported_format in supported_list {
                message.push_str(&format!("{supported_format}\n"));
            }
            Err(message)
        }
    }
}

/// TODO: gdb changes the default `format_specifier` every time x or p is used. For now we will use a static default of `x`.
impl Default for GdbNuf {
    fn default() -> Self {
        Self {
            unit_count: 1,
            unit_specifier: GdbUnit::Word,
            format_specifier: GdbFormat::Hex,
        }
    }
}

impl FromStr for GdbNuf {
    type Err = DebuggerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut nuf = value.to_string();
        let mut unit_specifier: Option<GdbUnit> = None;
        let mut format_specifier: Option<GdbFormat> = None;

        // Decode in reverse order, so that we can deal with variable 'count' characters.
        while let Some(last_char) = nuf.pop() {
            match last_char {
                't' | 'x' | 'i' | 'v' | 'n' => {
                    if format_specifier.is_none() {
                        format_specifier = Some(GdbFormat::try_from(&last_char)?);
                    } else {
                        return Err(DebuggerError::UserMessage(format!(
                            "Invalid format specifier: {value}"
                        )));
                    }
                }
                'b' | 'h' | 'w' | 'g' => {
                    if unit_specifier.is_none() {
                        unit_specifier = Some(GdbUnit::try_from(&last_char)?);
                    } else {
                        return Err(DebuggerError::UserMessage(format!(
                            "Invalid unit specifier: {value}"
                        )));
                    }
                }
                _ => {
                    if last_char.is_numeric() {
                        // The remainder of the string is the unit count.
                        nuf.push(last_char);
                        break;
                    } else {
                        return Err(DebuggerError::UserMessage(format!(
                            "Invalid '/Nuf' specifier: {value}"
                        )));
                    }
                }
            }
        }

        let mut result = Self::default();
        if let Some(format_specifier) = format_specifier {
            result.format_specifier = format_specifier;
        }
        if let Some(unit_specifier) = unit_specifier {
            result.unit_specifier = unit_specifier;
        }
        if !nuf.is_empty() {
            result.unit_count = nuf.parse::<usize>().map_err(|error| {
                DebuggerError::UserMessage(format!(
                    "Invalid unit count specifier: {value} - {error}"
                ))
            })?;
        }

        Ok(result)
    }
}

pub(crate) struct GdbNufMemoryResult<'a> {
    pub(crate) nuf: &'a GdbNuf,
    pub(crate) memory: &'a [u8],
}

impl Display for GdbNufMemoryResult<'_> {
    // TODO: Consider wrapping lines at 80 characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.nuf.format_specifier {
            GdbFormat::Binary => {
                let width = 10_usize;
                for byte in self.memory {
                    write!(f, "{byte:#0width$b} ")?;
                }
            }
            GdbFormat::Hex => {
                let width = 4_usize;
                for byte in self.memory {
                    write!(f, "{byte:#0width$x} ")?;
                }
            }
            _ => write!(f, "<cannot print>")?,
        }
        Ok(())
    }
}
