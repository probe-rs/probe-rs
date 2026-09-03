//! The JTAG bit-op path: init, the pack-mode probe and bit shifting.

use bitvec::vec::BitVec;

use crate::probe::DebugProbeError;

use super::capabilities::Pack;
use super::device::Ch347Device;

const CMD_JTAG_INIT: u8 = 0xD0;
const CMD_JTAG_BIT_OP_RD: u8 = 0xD2;

/// No firmware accepts this clock index; the reply byte tells the pack mode apart instead.
const PACK_PROBE_INDEX: u8 = 9;
/// Bit-op cycles per batch, two pin bytes each; the reply carries one byte per cycle.
const MAX_BITS_PER_FLUSH: usize = 127;

const PIN_TCK: u8 = 1 << 0;
const PIN_TMS: u8 = 1 << 1;
const PIN_TDI: u8 = 1 << 4;
/// TRST is push-pull and shares a pin with GPIO3; it stays high so JTAG traffic never resets a target.
const PIN_TRST: u8 = 1 << 5;
const JTAG_IDLE_PINS: u8 = PIN_TMS | PIN_TRST;

#[derive(Debug, Clone, Copy)]
pub(super) struct JtagCycle {
    tms: bool,
    tdi: bool,
    capture: bool,
}

impl JtagCycle {
    fn pins(self) -> u8 {
        (u8::from(self.tms) * PIN_TMS) | (u8::from(self.tdi) * PIN_TDI) | PIN_TRST
    }
}

impl Ch347Device {
    pub(super) fn jtag_init(&mut self, index: u8) -> Result<u8, DebugProbeError> {
        let pins = JTAG_IDLE_PINS;
        self.command_status(CMD_JTAG_INIT, &[0, index, pins, pins, pins, pins])
    }

    /// Runs the pack-mode probe once and remembers the answer.
    ///
    /// The JTAG init drives TRST high; SWD mode never needs it.
    pub(super) fn pack(&mut self) -> Result<Pack, DebugProbeError> {
        if let Some(pack) = self.pack {
            return Ok(pack);
        }
        let pack = match self.jtag_init(PACK_PROBE_INDEX)? {
            0 => Pack::Standard,
            _ => Pack::Larger,
        };
        tracing::debug!("{pack:?} pack mode");
        self.pack = Some(pack);
        Ok(pack)
    }

    pub(super) fn flush_jtag(&mut self) -> Result<(), DebugProbeError> {
        if self.jtag_queue.is_empty() {
            return Ok(());
        }
        let mut payload = Vec::with_capacity(self.jtag_queue.len() * 2);
        for cycle in &self.jtag_queue {
            let pins = cycle.pins();
            payload.push(pins);
            payload.push(pins | PIN_TCK);
        }
        let reply = self.command(CMD_JTAG_BIT_OP_RD, &payload, self.jtag_queue.len())?;
        for (cycle, &tdo) in self.jtag_queue.iter().zip(&reply) {
            if cycle.capture {
                self.jtag_captured.push(tdo & 1 == 1);
            }
        }
        self.jtag_queue.clear();
        Ok(())
    }

    pub(crate) fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture: bool,
    ) -> Result<(), DebugProbeError> {
        if self.jtag_queue.len() >= MAX_BITS_PER_FLUSH {
            self.flush_jtag()?;
        }
        self.jtag_queue.push(JtagCycle { tms, tdi, capture });
        Ok(())
    }

    pub(crate) fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush_jtag()?;
        Ok(std::mem::take(&mut self.jtag_captured))
    }
}

#[cfg(test)]
mod tests {
    use super::super::capabilities::Pack;
    use super::super::device::tests::{PACK_PROBE, device};

    #[test]
    fn pack_probe_runs_once() {
        let (mut dev, script) = device(&[(PACK_PROBE, &[0xD0, 1, 0, 1])]);
        assert_eq!(dev.pack().unwrap(), Pack::Larger);
        assert_eq!(dev.pack().unwrap(), Pack::Larger);
        assert!(script.finished());

        let (mut dev, _) = device(&[(PACK_PROBE, &[0xD0, 1, 0, 0])]);
        assert_eq!(dev.pack().unwrap(), Pack::Standard);
    }

    #[test]
    fn bit_ops_keep_trst_high_and_capture_tdo() {
        let (mut dev, script) =
            device(&[(&[0xD2, 4, 0, 0x22, 0x23, 0x30, 0x31], &[0xD2, 2, 0, 1, 0])]);
        dev.shift_bit(true, false, true).unwrap();
        dev.shift_bit(false, true, true).unwrap();
        let bits = dev.read_captured_bits().unwrap();
        assert_eq!(bits.iter().by_vals().collect::<Vec<_>>(), [true, false]);
        assert!(script.finished());
    }

    #[test]
    fn detach_flushes_queued_bits() {
        let (mut dev, script) = device(&[(&[0xD2, 2, 0, 0x22, 0x23], &[0xD2, 1, 0, 1])]);
        dev.shift_bit(true, false, false).unwrap();
        dev.detach().unwrap();
        assert!(script.finished());
    }
}
