//! 2-pin cJTAG (IEEE 1149.7, OScan1) through the J-Link firmware.
//!
//! With the cJTAG target interface selected, the firmware translates the JTAG I/O operations
//! to OScan1 once the target is activated. Test-Logic-Reset deactivates OScan1, so the target
//! is activated again after each one. The TAP is then physically in Run-Test/Idle with the
//! IDCODE selected, which is what Test-Logic-Reset leaves, so the clocks that keep the TAP in
//! Test-Logic-Reset are dropped until it leaves it.
//!
//! The activation sequence was taken from the USB traffic of the J-Link DLL (V8.44) with a
//! CC2650.

use super::{Command, JLink, JlinkError};
use crate::probe::jtag::FullTapState;

// Parameters of the undocumented `CJTAG_CONFIG` command.

/// Whether the JTAG I/O operations are translated to OScan1: 0 off, 1 on. Setting it puts
/// nothing on the wire. Replies 0.
const PARAM_TRANSLATE: u32 = 8;
/// The firmware sends a reset escape: about ten TMSC edges with TCKC high, ending with TMSC high.
/// The value is ignored. Replies 1.
const PARAM_RESET_ESCAPE: u32 = 9;
/// The firmware sends a check packet: four TCKC clocks with TMSC low, untranslated even with
/// translation on. The value is ignored. Replies 1.
const PARAM_CHECK_PACKET: u32 = 10;

#[derive(Debug)]
pub(super) struct CJtag {
    /// The TAP state that the bits sent so far lead to.
    state: FullTapState,
    /// The TAP was reset and activated: physically in Run-Test/Idle, while the bits sent so far
    /// lead to Test-Logic-Reset.
    parked: bool,
}

impl CJtag {
    pub(super) fn new() -> Self {
        Self {
            state: FullTapState::TestLogicReset,
            parked: false,
        }
    }
}

impl JLink {
    /// Whether a JTAG bit is sent in cJTAG mode. The bits that keep a parked TAP in
    /// Test-Logic-Reset or take it to Run-Test/Idle are not: it is already there.
    pub(super) fn cjtag_keep(&mut self, tms: bool, capture: bool) -> bool {
        if self.cjtag.parked && !capture {
            if !tms {
                self.cjtag.parked = false;
                self.cjtag.state = FullTapState::RunTestIdle;
            }
            return false;
        }
        self.cjtag.parked = false;
        self.cjtag.state = self.cjtag.state.step(tms);
        true
    }

    /// After a queued bit: if it took the TAP into Test-Logic-Reset, send the queue and
    /// activate the target again.
    pub(super) fn cjtag_after_shift(&mut self) -> Result<(), JlinkError> {
        if self.cjtag.state == FullTapState::TestLogicReset {
            self.flush_jtag()?;
            self.cjtag_activate(true)?;
        }
        Ok(())
    }

    /// Reset the TAP (unless `reset_sent`) and activate OScan1.
    pub(super) fn cjtag_activate(&mut self, reset_sent: bool) -> Result<(), JlinkError> {
        // 200 TMS ones, then one zero
        const ONES: [u8; 26] = {
            let mut ones = [0xff; 26];
            ones[25] = 0;
            ones
        };
        const ZEROS: [u8; 26] = [0; 26];
        // Online activation code, extension code and check packet (50 bits)
        const OAC: [u8; 7] = [0xad, 0x65, 0x59, 0x16, 0x16, 0x80, 0x01];
        const CP1: [u8; 3] = [0x01, 0xd6, 0x00];
        const CP2: [u8; 1] = [0x1b];

        if !reset_sent {
            self.jtag_io_raw(&[0x1f], &ZEROS, 5)?;
        }

        tracing::debug!("cJTAG activation");

        self.cjtag_config(PARAM_RESET_ESCAPE, 0)?;
        self.cjtag_config(PARAM_TRANSLATE, 0)?;
        self.jtag_io_raw(&ONES, &ZEROS, 201)?;

        // Selection escape: six TMSC edges while TCKC is high
        self.set_tck(false)?;
        self.set_tms(false)?;
        self.set_tck(true)?;
        for _ in 0..3 {
            self.set_tms(true)?;
            self.set_tms(false)?;
        }

        self.jtag_io_raw(&ZEROS, &ZEROS, 32)?;
        self.cjtag_config(PARAM_CHECK_PACKET, 0)?;
        self.jtag_io_raw(&OAC, &ZEROS, 50)?;
        self.cjtag_config(PARAM_CHECK_PACKET, 0)?;
        self.cjtag_config(PARAM_TRANSLATE, 1)?;
        self.jtag_io_raw(&CP1, &ZEROS, 18)?;
        self.cjtag_config(PARAM_CHECK_PACKET, 0)?;
        self.jtag_io_raw(&CP2, &ZEROS, 6)?;

        self.cjtag.state = FullTapState::TestLogicReset;
        self.cjtag.parked = true;
        Ok(())
    }

    /// The undocumented `CJTAG_CONFIG` command (0x0e).
    fn cjtag_config(&mut self, param: u32, value: u32) -> Result<u32, JlinkError> {
        let mut cmd = [0u8; 17];
        cmd[0] = Command::CJtagConfig as u8;
        cmd[1..5].copy_from_slice(&param.to_le_bytes());
        cmd[5..9].copy_from_slice(&value.to_le_bytes());
        // The firmware ignores this word.
        cmd[9..13].copy_from_slice(&4u32.to_le_bytes());
        self.write_cmd(&cmd)?;
        self.read_u32()
    }

    /// JTAG I/O outside of the queue and of the TAP state tracking (`HW_JTAG2`, no status).
    fn jtag_io_raw(&mut self, tms: &[u8], tdi: &[u8], bits: u16) -> Result<(), JlinkError> {
        let bytes = usize::from(bits).div_ceil(8);
        let mut cmd = Vec::with_capacity(4 + 2 * bytes);
        cmd.extend([Command::HwJtag2 as u8, 0]);
        cmd.extend(bits.to_le_bytes());
        cmd.extend(&tms[..bytes]);
        cmd.extend(&tdi[..bytes]);
        self.write_cmd(&cmd)?;

        let mut tdo = vec![0; bytes];
        self.read(&mut tdo)
    }
}
