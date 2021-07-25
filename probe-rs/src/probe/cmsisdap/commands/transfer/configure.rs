use super::super::{CommandId, Request, SendError, Status};

/// The DAP_TransferConfigure Command sets parameters for DAP_Transfer and DAP_TransferBlock.
#[derive(Debug)]
pub struct ConfigureRequest {
    /// Number of extra idle cycles after each transfer.
    pub idle_cycles: u8,
    /// Number of transfer retries after WAIT response.
    pub wait_retry: u16,
    /// Number of retries on reads with Value Match in DAP_Transfer. On value mismatch the Register is read again until its value matches or the Match Retry count exceeds.
    pub match_retry: u16,
}

impl Request for ConfigureRequest {
    const COMMAND_ID: CommandId = CommandId::TransferConfigure;

    type Response = ConfigureResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        use scroll::{Pwrite, LE};

        buffer[0] = self.idle_cycles;
        buffer
            .pwrite_with(self.wait_retry, 1, LE)
            .expect("This is a bug. Please report it.");
        buffer
            .pwrite_with(self.match_retry, 3, LE)
            .expect("This is a bug. Please report it.");
        Ok(5)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(ConfigureResponse(Status::from_byte(buffer[0])?))
    }
}

#[derive(Debug)]
pub struct ConfigureResponse(pub(crate) Status);
