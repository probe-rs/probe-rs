use super::super::{CommandId, Request, SendError};
use scroll::{Pread, LE};

pub struct EdbgGetConfigRequest {
    pub count: u8,
    pub config_id: u8,
    pub parameter: u8,
}

impl Request for EdbgGetConfigRequest {
    const COMMAND_ID: CommandId = CommandId::EdbgGetConfig;

    type Response = EdbgGetConfigResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = self.count;
        buffer[1] = self.config_id;
        buffer[2] = self.parameter;

        Ok(3)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let status = buffer[0];
        if status == 0 {
            let size: u16 = buffer.pread_with(1, LE).expect("Failed to read size");
            Ok(EdbgGetConfigResponse {
                config_packets: buffer[3..3 + size as usize].to_vec(),
            })
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

//pub struct EdbgConfigPacket {
//    config_id: u8,
//    data_type: u8,
//    data: Vec<u8>,
//}

pub struct EdbgGetConfigResponse {
    config_packets: Vec<u8>,
}
