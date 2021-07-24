use anyhow::{anyhow, Context, Result};
use std::fmt::Debug;
use std::ops::Deref;

use crate::DebugProbeError;

#[derive(Clone)]
pub struct ReceiveBuffer {
    data: Vec<u8>,
}

impl ReceiveBuffer {
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn get_payload(&self) -> Result<&[u8], DebugProbeError> {
        let start = self.iter().position(|&c| c == b'$');
        let end = self.iter().rposition(|&c| c == b'#');
        if let (Some(start), Some(end)) = (start, end) {
            Ok(&self[start + 1..end])
        } else {
            Err(anyhow!("Malformed ICDI response").into())
        }
    }

    pub fn check_cmd_result(&self) -> Result<(), DebugProbeError> {
        let payload = self.get_payload()?;
        if payload.is_empty() {
            return Err(anyhow!("Empty response payload").into());
        }
        if payload.starts_with(b"OK") {
            Ok(())
        } else {
            if payload[0] == b'E' {
                let err = std::str::from_utf8(&payload[1..3])
                    .context("Err HEX not UTF-8")
                    .map(|s| {
                        u8::from_str_radix(s, 16).with_context(|| {
                            format!("Error code decode error, {:?}", &payload[1..3])
                        })
                    })??;
                Err(anyhow!("ICDI command response contained error {}", err).into())
            } else {
                Ok(()) // assume ok
            }
        }
    }
}

impl Debug for ReceiveBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "buffer:[")?;
        for &c in &self[..] {
            if c.is_ascii() && !c.is_ascii_control() {
                write!(f, "{}", c as char)?;
            } else {
                write!(f, ",{},", c)?;
            }
        }
        write!(f, "]")
    }
}

impl Deref for ReceiveBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.data.as_slice()
    }
}
