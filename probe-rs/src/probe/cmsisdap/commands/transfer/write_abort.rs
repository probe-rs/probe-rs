use super::super::{CommandId, Request, SendError, Status};

/// The DAP_WriteABORT Command writes the ABORT register of the debug port.
///
/// Over JTAG, ABORT has its own instruction, and DAP_Transfer cannot write it.
#[derive(Debug, Copy, Clone)]
pub struct WriteAbortRequest {
    /// The index of the debug port in the JTAG chain. SWD ignores it.
    pub dap_index: u8,
    /// The value to write to ABORT.
    pub value: u32,
}

impl Request for WriteAbortRequest {
    const COMMAND_ID: CommandId = CommandId::WriteAbort;

    type Response = WriteAbortResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.dap_index;
        buffer[1..5].copy_from_slice(&self.value.to_le_bytes());
        Ok(5)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(WriteAbortResponse {
            status: Status::from_byte(buffer[0])?,
        })
    }
}

#[derive(Debug)]
pub struct WriteAbortResponse {
    pub(crate) status: Status,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_the_dap_index_and_the_value() {
        let request = WriteAbortRequest {
            dap_index: 1,
            value: 0x0000_001e,
        };
        let mut buffer = [0u8; 8];
        assert_eq!(request.to_bytes(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer[..5], &[0x01, 0x1e, 0x00, 0x00, 0x00]);
    }
}
