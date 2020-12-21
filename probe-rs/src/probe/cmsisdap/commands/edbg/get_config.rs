use super::super::{Category, Request, Response, Result};
use scroll::{Pread, LE};
use anyhow::anyhow;


pub struct EdbgGetConfigRequest {
    pub count: u8,
    pub config_id: u8,
    pub parameter: u8,
}

impl Request for EdbgGetConfigRequest {
    const CATEGORY: Category = Category(0x83);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = self.count;
        buffer[offset+1] = self.config_id;
        buffer[offset+2] = self.parameter;

        Ok(3)
    }
}

pub struct EdbgConfigPacket {
    config_id: u8,
    data_type: u8,
    data: Vec<u8>,
}

pub struct EdbgGetConfigResponse {
    config_packets: Vec<u8>,
}
impl Response for EdbgGetConfigResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let status = buffer[offset];
        if status == 0 {
            let size: u16 = buffer.pread_with(offset+1, LE)
                .expect("Failed to read size");
            Ok(EdbgGetConfigResponse{
                config_packets: buffer[offset+3 .. offset+3+size as usize].to_vec()
            })
        }
        else {
            Err(anyhow!("GET_CONFIG failed"))
        }
    }
}

