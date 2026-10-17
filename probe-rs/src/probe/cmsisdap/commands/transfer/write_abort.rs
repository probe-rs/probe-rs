use super::super::{CommandId, Request, SendError, Status};

/// The DAP_WriteABORT command writes the ABORT register of the selected debug port.
///
/// Unlike a `DAP_Transfer` write to DP address 0, this reaches ABORT in JTAG mode too, where
/// ABORT is a separate instruction rather than a DPACC register.
#[derive(Debug, Copy, Clone)]
pub struct WriteAbortRequest {
    /// The index of the DAP in the JTAG chain. Ignored in SWD mode.
    pub dap_index: u8,
    /// The value to write to ABORT.
    pub abort: u32,
}

impl Request for WriteAbortRequest {
    const COMMAND_ID: CommandId = CommandId::WriteAbort;

    type Response = WriteAbortResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.dap_index;
        buffer[1..5].copy_from_slice(&self.abort.to_le_bytes());
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
    use super::WriteAbortRequest;
    use crate::probe::cmsisdap::commands::Request;

    #[test]
    fn the_request_is_the_dap_index_and_the_value() {
        let request = WriteAbortRequest {
            dap_index: 1,
            abort: 0x0000_001e,
        };
        let mut buffer = [0u8; 8];
        assert_eq!(request.to_bytes(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer[..5], &[0x01, 0x1e, 0x00, 0x00, 0x00]);
    }
}
