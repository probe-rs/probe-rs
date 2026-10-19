use super::super::{CommandId, Request, SendError, Status};

/// The DAP_WriteABORT command writes the ABORT register of the debug port.
///
/// On JTAG the ABORT register is behind its own instruction, which DAP_Transfer cannot reach.
#[derive(Debug, Copy, Clone)]
pub struct WriteAbortRequest {
    /// The index of the debug port in the JTAG chain, ignored for SWD.
    pub dap_index: u8,
    /// The value written to ABORT.
    pub value: u32,
}

impl Request for WriteAbortRequest {
    const COMMAND_ID: CommandId = CommandId::WriteAbort;

    type Response = Status;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.dap_index;
        buffer[1..5].copy_from_slice(&self.value.to_le_bytes());
        Ok(5)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Status::from_byte(buffer[0])
    }
}
