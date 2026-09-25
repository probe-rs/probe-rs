use std::fmt::{self, Display, Write as _};

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::core_ops::{WireRegisterId, WireRegisterValue};
use crate::{Key, NoResponse, RpcResult, Session};

#[derive(Serialize, Deserialize, Schema)]
pub struct StackTraces {
    pub cores: Vec<StackTrace>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct StackTrace {
    pub core: u32,
    pub frames: Vec<StackTraceFrame>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct StackTraceFrame {
    pub function_name: String,
    pub program_counter: u64,
    pub is_inlined: bool,
    pub location: Option<SourceLocation>,
}

impl Display for StackTraceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output_stream = String::new();
        write!(f, "{} @ {:x}", self.function_name, self.program_counter).unwrap();

        if self.is_inlined {
            write!(&mut output_stream, " inline").unwrap();
        }
        f.write_str("\n")?;

        if let Some(location) = &self.location {
            write!(f, "       {location}")?;
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct SourceLocation {
    pub file: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

impl Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(column) = self.column {
                write!(f, ":{column}")?;
            }
        }
        Ok(())
    }
}

/// Path-based stack trace for generic CLI callers. Parses DWARF from `path`
/// on each request and does not use session `ServerDebugState`.
#[derive(Serialize, Deserialize, Schema)]
pub struct TakeStackTraceRequest {
    pub sessid: Key<Session>,
    pub path: String,
    pub stack_frame_limit: u32,
}

/// Session-owned rich stack trace for DAP and other server-state consumers.
/// Requires preloaded `ServerDebugState` via `load_debug_info`; never
/// accepts or parses a binary path.
#[derive(Serialize, Deserialize, Schema)]
pub struct TakeRichStackTraceRequest {
    pub sessid: Key<Session>,
    /// When set, only unwind this core. When omitted, every enabled core is
    /// unwound.
    pub core: Option<u32>,
    pub stack_frame_limit: u32,
}

pub type TakeStackTraceResponse = RpcResult<StackTraces>;

#[derive(Serialize, Deserialize, Schema)]
pub struct LoadDebugInfoRequest {
    pub sessid: Key<Session>,
    pub path: String,
}

pub type LoadDebugInfoResponse = NoResponse;

/// A single register, in the wire format used by the rich stack trace.
#[derive(Serialize, Deserialize, Schema, Clone, PartialEq)]
pub struct WireDebugRegister {
    pub id: WireRegisterId,
    pub dwarf_id: Option<u16>,
    pub value: Option<WireRegisterValue>,
}

/// A stack frame carrying the full per-frame register state plus frame
/// metadata. The server owns the `local_variables`/`static_variables`
/// `VariableCache` trees (keyed by `sessid` + core); the client relays the
/// server-assigned `id` handles
/// verbatim so subsequent `scopes`/`variables` requests resolve server-side.
#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RichStackTraceFrame {
    pub function_name: String,
    pub program_counter: WireRegisterValue,
    pub is_inlined: bool,
    pub location: Option<SourceLocation>,
    pub frame_base: Option<u64>,
    pub canonical_frame_address: Option<u64>,
    pub registers: Vec<WireDebugRegister>,
    /// Server-assigned frame id (also the DAP `frameId` and the registers
    /// scope `variablesReference`).
    pub id: u32,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RichStackTrace {
    pub core: u32,
    pub frames: Vec<RichStackTraceFrame>,
}

#[derive(Serialize, Deserialize, Schema, Clone)]
pub struct RichStackTraces {
    pub cores: Vec<RichStackTrace>,
}

pub type TakeRichStackTraceResponse = RpcResult<RichStackTraces>;

#[cfg(test)]
mod tests {
    use super::*;
    use postcard::to_allocvec;

    #[test]
    fn rich_stack_trace_request_has_no_path_field() {
        let request = TakeRichStackTraceRequest {
            sessid: Key::test(1),
            core: Some(0),
            stack_frame_limit: 64,
        };
        let encoded = to_allocvec(&request).unwrap();
        let decoded: TakeRichStackTraceRequest = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded.core, Some(0));
        assert_eq!(decoded.stack_frame_limit, 64);
    }
}
