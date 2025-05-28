//! Protocol definitions. These constants correspond to the implementation of the `probe-rs` and
//! `swd-probe` applets, sometimes in a non-obvious way. They must be changed in sync with
//! the applets.
//!
//! The `probe-rs` applet is composite: it contains multiple endpoints that can be accessed
//! independently, using COBS as a framing method. The first byte of each packet is the target
//! address (corresponding to the [`Target`] enum), the rest is the packet data. The endpoints are
//! not aware of packet boundaries: two 1-byte and one 2-byte (with the same values) packets are
//! processed exactly the same aside from timing.
//!
#![allow(unused)]

/// Target address. The numeric values correspond to packet header bytes for that target.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Root = 0x00,
    Swd = 0x01,
}

pub mod root {
    pub const IDENTIFIER: &[u8; 12] = b"probe-rs,v00";

    pub const CMD_IDENTIFY: u8 = 0x00;
    pub const CMD_GET_DIVISOR: u8 = 0x10;
    pub const CMD_SET_DIVISOR: u8 = 0x20;
    pub const CMD_ASSERT_RESET: u8 = 0x30;
    pub const CMD_CLEAR_RESET: u8 = 0x31;

    pub const REF_CLK_FREQUENCY: u32 = 24_000_000;

    pub fn divisor_to_frequency(divisor: u16) -> u32 {
        REF_CLK_FREQUENCY / (divisor as u32 + 1)
    }

    pub fn frequency_to_divisor(frequency: u32) -> u16 {
        (REF_CLK_FREQUENCY.div_ceil(frequency) - 1) as u16
    }
}

pub mod swd {
    pub const CMD_TRANSFER: u8 = 0x00;
    pub const CMD_SEQUENCE: u8 = 0x20;

    pub const SEQ_LEN_MASK: u8 = 0x1f;

    pub const RSP_ACK_MASK: u8 = 0x07;
    pub const RSP_ACK_OK: u8 = 0b001;
    pub const RSP_ACK_WAIT: u8 = 0b010;
    pub const RSP_ACK_FAULT: u8 = 0b100;

    pub const RSP_TYPE_MASK: u8 = 0x30;
    pub const RSP_TYPE_DATA: u8 = 0x00;
    pub const RSP_TYPE_NO_DATA: u8 = 0x10;
    pub const RSP_TYPE_ERROR: u8 = 0x20;
}

impl TryFrom<u8> for Target {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Root),
            1 => Ok(Self::Swd),
            _ => Err(()),
        }
    }
}
