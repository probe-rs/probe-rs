use crate::cmd::dap_server::debug_adapter::dap::request_helpers::disassemble_target_memory;
use postcard_rpc::header::VarHeader;
use probe_rs_rpc::disassemble::{
    DisassembleRequest, DisassembleResponse, WireDisassembledInstruction, WireSource,
};

use crate::rpc::functions::{RpcContext, convert::lift};

/// Disassemble target memory server-side, running the capstone disassembly
/// (shared with the local path via `disassemble_target_memory`) against the
/// live `Core` and the cached server-side `DebugInfo`. The client only
/// relays the request and reconstructs the DAP `DisassembledInstruction`
/// (with the always-`None` fields `end_column`/`end_line`/`symbol`/
/// `presentation_hint` defaulted).
pub async fn disassemble(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: DisassembleRequest,
) -> DisassembleResponse {
    // Without DWARF the instructions are still disassembled; they just carry
    // no source locations.
    let debug_info = ctx
        .with_server_debug_state(request.sessid, |state| state.debug_info.clone())
        .await;

    let mut session = ctx.session(request.sessid).await;
    let mut core = lift(session.core(request.core as usize))?;
    let instructions = disassemble_target_memory(
        &mut core,
        debug_info.as_deref(),
        request.instruction_offset,
        request.byte_offset,
        request.memory_reference,
        request.instruction_count,
    )
    .map_err(|e| e.to_string())?;
    Ok(instructions
        .into_iter()
        .map(WireDisassembledInstruction::from)
        .collect())
}

pub(crate) mod convert {
    use super::{WireDisassembledInstruction, WireSource};
    use crate::cmd::dap_server::debug_adapter::dap::dap_types::{DisassembledInstruction, Source};

    impl From<Source> for WireSource {
        fn from(s: Source) -> Self {
            WireSource {
                name: s.name,
                path: s.path,
            }
        }
    }

    impl From<DisassembledInstruction> for WireDisassembledInstruction {
        fn from(i: DisassembledInstruction) -> Self {
            WireDisassembledInstruction {
                address: i.address,
                column: i.column,
                instruction: i.instruction,
                instruction_bytes: i.instruction_bytes,
                line: i.line,
                location: i.location.map(WireSource::from),
            }
        }
    }
}
