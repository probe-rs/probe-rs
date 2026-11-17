//! 2-pin cJTAG (IEEE 1149.7, OScan1) on the XDS110.
//!
//! The first `CJTAG_CONNECT` with the OScan1 format after `XDS_CONNECT` and clocks with TMS
//! high activates the target, from any format. Another one, or one after a TAP reset, may lose
//! the target, so an activation connects the probe again first.
//!
//! Test-Logic-Reset brings the target back to JScan0, so it is activated again after each one,
//! and a TMS sequence through it is followed but not sent.
//! The TAP is then in Run-Test/Idle, with the IDCODE instruction that Test-Logic-Reset leaves:
//! it is parked there until it leaves Test-Logic-Reset. Path moves are refused in cJTAG mode.

use super::{CJTAG_CONNECT, OCD_SCAN_REQUEST, XDS_CONNECT, XDS_DISCONNECT, Xds110, jtag::XdsState};
use crate::probe::{DebugProbeError, TapState};

/// `CJTAG_CONNECT` format for OScan1.
const MODE_OSCAN1: u32 = 7;

/// The cJTAG targets are TI chips, with an ICEPick alone on the chain after a TAP reset.
const ICEPICK_IR_LEN: u8 = 6;
const ICEPICK_IDCODE: u8 = 0x04;

impl Xds110 {
    /// Activates OScan1 after the line reset of an attach. The TAP is parked in
    /// Test-Logic-Reset afterwards.
    pub(super) fn cjtag_attach(&mut self) -> Result<(), DebugProbeError> {
        self.command_u32(CJTAG_CONNECT, MODE_OSCAN1)?;
        // Loads IDCODE, as Test-Logic-Reset does, and stays in Run-Test/Idle.
        self.ir_scan(ICEPICK_IDCODE)?;
        self.jtag_state.loaded_ir = None;
        self.tms_state = Some(XdsState::Idle);
        self.jtag_state.tap_state = TapState::TestLogicReset;
        Ok(())
    }

    /// Activates OScan1 again. The TAP is parked in Test-Logic-Reset afterwards.
    pub(super) fn cjtag_activate(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("XDS110: cJTAG activation");
        self.command(&[XDS_DISCONNECT])?;
        self.command(&[XDS_CONNECT])?;
        self.reset_lines()?;
        self.cjtag_attach()
    }

    /// Follows a TMS sequence, which can't be sent in cJTAG mode, and returns the state it
    /// leads to. A sequence into Test-Logic-Reset activates the target again; one that ends
    /// between stable states resets the TAP.
    pub(super) fn cjtag_sequence(
        &mut self,
        bits: impl Iterator<Item = bool>,
    ) -> Result<TapState, DebugProbeError> {
        let mut state = XdsState::from(self.jtag_state.tap_state);
        let mut reset = false;
        for tms in bits {
            let next = state.next(tms);
            reset |= next == XdsState::Reset && state != XdsState::Reset;
            state = next;
        }
        let Some(end) = state.stable() else {
            self.cjtag_activate()?;
            return Ok(TapState::TestLogicReset);
        };
        if reset {
            self.cjtag_activate()?;
        }
        if end != self.jtag_state.tap_state {
            self.goto_state(state)?;
        }
        Ok(end)
    }

    /// Loads an ICEPick instruction, from and to Run-Test/Idle.
    fn ir_scan(&mut self, instruction: u8) -> Result<(), DebugProbeError> {
        let payload = [
            OCD_SCAN_REQUEST,
            super::jtag::CMD_IR_SCAN,
            XdsState::Idle as u8,
            ICEPICK_IR_LEN,
            0,
            instruction,
            0,
        ];
        self.usb.execute(&payload, 1)?;
        Ok(())
    }
}
