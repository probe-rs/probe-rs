use std::io::Write;

use anyhow::anyhow;
use hex::FromHex;

use crate::probe::ti_icdi::receive_buffer::ReceiveBuffer;
use crate::DebugProbeError;

pub trait GdbRemoteInterface {
    fn get_max_packet_size(&self) -> usize;

    fn read_reg(&mut self, regsel: u32) -> Result<u32, DebugProbeError> {
        let mut buf = Vec::with_capacity(10);
        write!(&mut buf, "p{:x}", regsel).unwrap();
        let buf = self.send_command(&buf)?;
        buf.check_cmd_result()?;
        let x = buf.get_payload()?;
        log::trace!("read reg response {:?}", x);
        let y = <[u8; 4]>::from_hex(x)
            .map_err(|_| DebugProbeError::Other(anyhow!("Hex conversion failed {:?}", buf)))?;

        Ok(u32::from_le_bytes(y))
    }

    fn write_reg(&mut self, regsel: u32, val: u32) -> Result<(), DebugProbeError> {
        let mut buf = Vec::with_capacity(20);
        write!(&mut buf, "P{:x}={:08x}", regsel, val.to_be()).unwrap();
        self.send_command(&buf)?.check_cmd_result()
    }

    fn read_mem(&mut self, mut addr: u32, data: &mut [u8]) -> Result<(), DebugProbeError> {
        let mut buf = Self::new_send_buffer(20);
        for chunk in data.chunks_mut(self.get_max_packet_size() / 2 - 7) {
            buf.clear();
            write!(&mut buf, "$x{:08x},{:08x}", addr, chunk.len()).unwrap();
            let response = self.send_packet(&mut buf)?;
            response.check_cmd_result()?;

            let mut escaped = false;
            let mut byte_cnt = 0;
            response
                .get_payload()?
                .strip_prefix(b"OK:")
                .ok_or(DebugProbeError::Other(anyhow!("OK: missing")))?
                .iter()
                .filter_map(|&ch| {
                    if escaped {
                        escaped = false;
                        Some(ch ^ 0x20)
                    } else if ch == b'}' {
                        escaped = true;
                        None
                    } else {
                        Some(ch)
                    }
                })
                .zip(chunk.iter_mut())
                .for_each(|(a, b)| {
                    byte_cnt += 1;
                    *b = a;
                });
            if byte_cnt != chunk.len() {
                Err(DebugProbeError::Other(anyhow!("Short read")))?;
            }
            addr += chunk.len() as u32;
        }
        Ok(())
    }

    fn write_mem(&mut self, mut addr: u32, data: &[u8]) -> Result<(), DebugProbeError> {
        let max = self.get_max_packet_size();
        let mut cur = data.iter();
        let mut buf = Vec::with_capacity(max);
        while cur.len() > 0 {
            buf.clear();
            let mut len = 0;
            let curr_addr = addr;
            write!(&mut buf, "$X{:08x},{:08x}:", addr, 0).unwrap(); // Placeholder
            while buf.len() - 5 <= max {
                // 5, since the next byte might be 2, and 3 for # and checksum
                if let Some(byte) = cur.next() {
                    match byte {
                        b'$' | b'#' | b'}' | b'*' => {
                            buf.push(b'}');
                            buf.push(byte ^ 0x20);
                        }
                        _ => buf.push(*byte),
                    }
                    addr += 1;
                    len += 1;
                } else {
                    break;
                }
            }
            write!(&mut buf[0..], "$X{:08x},{:08x}:", curr_addr, len).unwrap();
            self.send_packet(&mut buf)?.check_cmd_result()?;
        }
        Ok(())
    }

    fn send_remote_command(&mut self, cmd: &[u8]) -> Result<ReceiveBuffer, DebugProbeError> {
        let mut buf = Self::new_send_buffer(cmd.len() * 2 + 6);
        buf.extend_from_slice(b"qRcmd,");
        for c in cmd {
            write!(buf, "{:02x}", c).unwrap();
        }
        self.send_packet(&mut buf)
    }

    fn send_command(&mut self, cmd: &[u8]) -> Result<ReceiveBuffer, DebugProbeError> {
        let mut buf = Self::new_send_buffer(cmd.len());
        buf.extend_from_slice(cmd);
        self.send_packet(&mut buf)
    }

    fn new_send_buffer(capacity: usize) -> Vec<u8> {
        let mut b = Vec::with_capacity(capacity + 4);
        b.push(b'$');
        b
    }

    fn send_packet(&mut self, data: &mut Vec<u8>) -> Result<ReceiveBuffer, DebugProbeError>;
}
