//! Sequences for cc13xx_cc26xx devices

use crate::architecture::arm::{ArmError, DapError, DapProbe};
use crate::probe::common::JtagState;
use crate::probe::{DebugProbeError, JtagAccess, JtagSequence, WireProtocol};
use bitvec::field::BitField;
use bitvec::vec::BitVec;
use probe_rs_target::ScanChainElement;

/// A TI ICEPick device. An ICEPick manages a JTAG device and can be used to add or
/// remove JTAG TAPs from a bus.
#[derive(Debug)]
pub struct Icepick<'a> {
    probe: &'a mut dyn JtagAccess,
}

// IR register values, see <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf> table 6-7
const IR_ROUTER: u32 = 0x02;
const IR_CONNECT: u32 = 0x07;
const IR_BYPASS: u32 = 0x3F;
const IR_LEN_IN_BITS: u8 = 6;

/// Write to register 0 in the Debug TAP linking block (Section 6.3.4.3)
/// Namely:
/// * [20]   : `InhibitSleep`
/// * [16:14]: `ResetControl == Normal`
/// * [8]    : `SelectTAP == 1`
/// * [6]    : `ForcePower == Keep target on`
/// * [3]    : `ForceActive == Enable clocks`
const SD_TAP_DEFAULT: u32 = (1 << 20) | (1 << 8) | (1 << 6) | (1 << 3);

/// Default values for SYSCTRL
/// * [7] KEEPPOWEREDINTLR - Don't reset the ICEPICK in JTAG Test-Logic Reset
const SYSCTRL_DEFAULT: u32 = 0x80;

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
enum IcepickRoutingRegister {
    /// Control the ICEPick itself
    Sysctrl = 1,
    /// Modify parameters of a specific Secondary Tap
    SdTap(u8),
}

#[repr(u32)]
#[allow(dead_code)]
#[derive(Debug)]
pub enum ResetControl {
    /// Reset and run
    Normal = 0,
    /// Wait in reset until RELEASEFROMWIR is asserted
    WaitInReset = 1,
    /// Prevent a reset from occurring
    BlockReset = 2,
    /// Assert the warm reset signal and keep it there
    AssertAndHold = 3,
}

impl From<IcepickRoutingRegister> for u32 {
    fn from(value: IcepickRoutingRegister) -> Self {
        match value {
            IcepickRoutingRegister::Sysctrl => 1u32,
            IcepickRoutingRegister::SdTap(tap) => 0b010_0000 | tap as u32,
        }
    }
}

impl<'a> Icepick<'a> {
    /// Create a new ICEPick interface. An ICEPick is a mux that sits on the JTAG bus
    /// and must be asked to enable various parts on the bus in order to allow us to
    /// talk to them. By default, the ICEPick will disable all secondary TAPs.
    pub fn new(interface: &'a mut dyn DapProbe) -> Result<Self, ArmError> {
        let probe = interface.try_as_jtag_probe().ok_or_else(|| {
            tracing::error!("Couldn't get probe as JtagAccess");
            ArmError::Dap(DapError::Protocol(WireProtocol::Jtag))
        })?;

        let mut this = Icepick { probe };

        // Reset the JTAG bus, which will remove all TAPs except the main ICEPICK.
        this.probe.tap_reset().map_err(ArmError::Probe)?;
        let tap_count = this.scan_jtag()?;
        if tap_count == 0 {
            tracing::error!("No TAP devices found!");
            return Err(ArmError::Probe(DebugProbeError::TargetNotFound));
        }

        // Update the scan chain to just have the one ICEPICK device.
        this.probe
            .set_scan_chain(&[ScanChainElement {
                name: Some("ICEPICK".to_owned()),
                ir_len: Some(IR_LEN_IN_BITS),
            }])
            .inspect_err(|e| tracing::error!("Couldn't set scan chain: {e}"))?;
        this.probe.select_target(0)?;

        // Enable write by setting the `ConnectKey` to 0b1001 (0x9) as per TRM section 6.3.3
        this.probe
            .write_register(IR_CONNECT, &[0x89], 8)
            .inspect_err(|e| tracing::error!("Couldn't write IR_CONNECT: {e}"))?;

        // Write to register 1 in the ICEPICK control block - keep JTAG powered in test logic reset
        this.icepick_router(IcepickRoutingRegister::Sysctrl, SYSCTRL_DEFAULT)?;

        Ok(this)
    }

    /// Print a list of IDCODEs on the JTAG bus.
    /// Note: this is currently waiting on upstream to merge
    /// https://github.com/probe-rs/probe-rs/pull/3590
    fn scan_jtag(&mut self) -> Result<u8, ArmError> {
        let mut tap_count = 0;
        tracing::trace!("Scan of JTAG bus:");
        // Enter SHIFT_DR state.
        // [IDLE] -> SELECT_DR_SCAN -> CAPTURE_DR -> SHIFT_DR
        // self.interface.swj_sequence(3, 0b001)?;
        for bit in &[true, false, false] {
            let mut data = BitVec::new();
            data.push(false);
            self.probe.shift_raw_sequence(JtagSequence {
                tdo_capture: false,
                tms: *bit,
                data,
            })?;
        }

        // Keep reading IDCODEs out until we get zeroes back.
        // let probe = self.jtag_probe()?;
        for index in 0..256 {
            let mut data = BitVec::new();
            for _ in 0..32 {
                data.push(false);
            }
            let idcode = self.probe.shift_raw_sequence(JtagSequence {
                tdo_capture: true,
                tms: false,
                data,
            })?;
            let idcode = idcode.load_be::<u32>();

            tracing::trace!("    TAP index {index}: 0x{idcode:08x}");
            if idcode == 0 {
                break;
            }
            tap_count += 1;
        }

        // Go back to IDLE state
        // [SHIFT_DR] -> EXIT1_DR -> UPDATE_DR -> IDLE
        // self.interface.swj_sequence(3, 0b011)?;
        for bit in &[true, true, false] {
            let mut data = BitVec::new();
            data.push(false);
            self.probe.shift_raw_sequence(JtagSequence {
                tdo_capture: false,
                tms: *bit,
                data,
            })?;
        }

        Ok(tap_count)
    }

    /// Reads or writes a given register using the ICEPICK router
    ///
    /// This function is used to load the router register of the ICEPICK TAP
    /// and connect a given data register to the TDO.
    ///
    /// This is a direct port from the openocd implementation:
    /// <https://github.com/openocd-org/openocd/blob/master/tcl/target/icepick.cfg#L56-L70>
    ///
    /// * `register`  - The register to access
    /// * `payload`   - The data to write to the register
    fn icepick_router(
        &mut self,
        register: IcepickRoutingRegister,
        payload: u32,
    ) -> Result<(), ArmError> {
        // The current implementation only supports register writes.
        let rw = 1;

        // Build the DR value based on the requested operation. The DR value
        // is based on the input arguments and contains several bitfields
        let dr = (rw << 31) | (u32::from(register) << 24) | (payload & 0xFFFFFF);

        self.probe
            .write_register(IR_ROUTER, &dr.to_le_bytes(), 32)?;

        let result = self
            .probe
            .write_register(IR_ROUTER, &0u32.to_le_bytes(), 32)?;
        tracing::trace!(
            "Value of {register:02x?}: 0x{:08x}",
            result.load_le::<u32>()
        );
        Ok(())
    }

    /// Does setup of the ICEPICK
    ///
    /// This will setup the ICEPICK to have the CPU/DAP on the scan chain and
    /// also power and enable the debug interface for use with probe-rs. The ICEPick
    /// will be placed in BYPASS mode, and only the selected `secondary_tap` will be
    /// present on the scan chain.
    ///
    /// This is a direct port of the openocd implementation:
    /// <https://github.com/openocd-org/openocd/blob/master/tcl/target/icepick.cfg#L81-L124>
    /// A few things were removed to fit the cc13xx_cc26xx family.
    pub(crate) fn select_tap(&mut self, secondary_tap: u8) -> Result<(), ArmError> {
        tracing::trace!("Selecting secondary tap {secondary_tap}");
        self.icepick_router(IcepickRoutingRegister::SdTap(secondary_tap), SD_TAP_DEFAULT)?;

        // Stay in Run/Test Idle for at least three cycles to activate the TAP
        self.probe.set_idle_cycles(3)?;
        // Enter the bypass state to remove the ICEPick from the scan chain.
        // This will insert three cycles after the configuration in order to make
        // the target TAP appear.
        self.probe.read_register(IR_BYPASS, 1)?;
        self.probe.set_idle_cycles(0)?;

        self.probe
            .set_scan_chain(&[
                ScanChainElement {
                    name: Some("TMS570".to_owned()),
                    ir_len: Some(4),
                },
                ScanChainElement {
                    name: Some("ICEPICK".to_owned()),
                    ir_len: Some(IR_LEN_IN_BITS),
                },
            ])
            .inspect_err(|e| tracing::error!("Couldn't set scan chain: {e}"))?;

        tracing::trace!("Should be active now");
        self.scan_jtag()?;

        Ok(())
    }

    /// Disable "Compact JTAG" support and enable full JTAG.
    pub(crate) fn ctag_to_jtag(&mut self) -> Result<(), ArmError> {
        // // Load IR with BYPASS
        // self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)?;

        // // cJTAG: Open Command Window
        // // This is described in section 6.2.2.1 of this document:
        // // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // // Also refer to the openocd implementation:
        // // <https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35>
        // self.zero_bit_scan()?;
        // self.zero_bit_scan()?;
        // self.shift_dr(1, 0x01, JtagState::RunTestIdle)?;

        // // cJTAG: Switch to 4 pin
        // // This is described in section 6.2.2.2 of this document:
        // // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // // Also refer to the openocd implementation:
        // // <https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35>
        // self.shift_dr(2, set_n_bits(2), JtagState::RunTestIdle)?;
        // self.shift_dr(9, set_n_bits(9), JtagState::RunTestIdle)?;

        // // Load IR with BYPASS so that future state transitions don't affect IR
        // self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)?;

        Ok(())
    }

    /// Load IR with BYPASS so that future state transitions don't affect IR
    pub(crate) fn bypass(&mut self) -> Result<(), ArmError> {
        // self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)
        Ok(())
    }
}
