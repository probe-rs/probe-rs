use std::fmt::{self, Display, Write as _};

use crate::rpc::{
    Key,
    functions::{RpcContext, RpcResult},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{Error, Session};
use probe_rs_debug::{DebugInfo, DebugRegisters, StackFrame, exception_handler_for_core};
use serde::{Deserialize, Serialize};

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

impl From<StackFrame> for StackTraceFrame {
    fn from(frame: StackFrame) -> Self {
        StackTraceFrame::from(&frame)
    }
}

impl From<&StackFrame> for StackTraceFrame {
    fn from(frame: &StackFrame) -> Self {
        StackTraceFrame {
            function_name: frame.function_name.clone(),
            program_counter: frame.pc.try_into().unwrap_or(0),
            is_inlined: frame.is_inlined,
            location: frame
                .source_location
                .as_ref()
                .map(|location| SourceLocation {
                    file: location.path.to_path().display().to_string(),
                    line: location.line,
                    column: location.column.map(|col| match col {
                        probe_rs_debug::ColumnType::LeftEdge => 1,
                        probe_rs_debug::ColumnType::Column(c) => c,
                    }),
                }),
        }
    }
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

#[derive(Serialize, Deserialize, Schema)]
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

#[derive(Serialize, Deserialize, Schema)]
pub struct TakeStackTraceRequest {
    pub sessid: Key<Session>,
    pub path: String,
    pub stack_frame_limit: u32,
}

pub type TakeStackTraceResponse = RpcResult<StackTraces>;

pub async fn take_stack_trace(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: TakeStackTraceRequest,
) -> TakeStackTraceResponse {
    let mut session = ctx.session(request.sessid).await;

    let Some(debug_info) = DebugInfo::from_file(&request.path).ok() else {
        Err("No debug info found.")?
    };

    session
        .halted_access(|session| {
            let mut cores = Vec::new();
            for (idx, core_type) in session.list_cores() {
                let mut core = match session.core(idx) {
                    Ok(core) => core,
                    Err(Error::CoreDisabled(_)) => continue,
                    Err(e) => return Err(e),
                };

                let initial_registers = DebugRegisters::from_core(&mut core);
                let exception_interface = exception_handler_for_core(core_type);
                let instruction_set = core.instruction_set().ok();
                let stack_frames = debug_info.unwind(
                    &mut core,
                    initial_registers,
                    exception_interface.as_ref(),
                    instruction_set,
                    request.stack_frame_limit as usize,
                )?;

                let mut frames = vec![];
                for frame in stack_frames.into_iter() {
                    frames.push(StackTraceFrame::from(frame));
                }

                cores.push(StackTrace {
                    core: idx as u32,
                    frames,
                });
            }
            Ok(StackTraces { cores })
        })
        .map_err(Into::into)
}
