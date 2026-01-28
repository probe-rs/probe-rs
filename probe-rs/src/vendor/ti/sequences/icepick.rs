//! Controls for the ICEPICK JTAG mux used on some TI parts

use crate::architecture::arm::{ArmError, DapError, DapProbe};
use crate::probe::{DebugProbeError, JtagAccess, JtagSequence, WireProtocol};
use bitvec::field::BitField;
use bitvec::vec::BitVec;
use probe_rs_target::ScanChainElement;

/// Which connection type is used by the Icepick
#[derive(Debug, PartialEq)]
pub enum DefaultProtocol {
    /// cJTAG two-wire variant
    CJtag,
    /// Standard JTAG implementation
    Jtag,
}

/// A TI ICEPick device. An ICEPick manages a JTAG device and can be used to add or
/// remove JTAG TAPs from a bus.
#[derive(Debug)]
pub struct Icepick<'a> {
    probe: &'a mut dyn JtagAccess,
}

// IR register values, see <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf> table 6-7
const IR_ROUTER: u32 = 0x02;
const IR_IDCODE: u32 = 0x04;
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

#[derive(PartialEq)]
enum JtagOperation {
    ShiftDr = 0x03,
    ShiftIr = 0x04,
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
    pub fn new(
        interface: &'a mut dyn DapProbe,
        protocol: DefaultProtocol,
    ) -> Result<Self, ArmError> {
        let probe = interface.try_as_jtag_probe().ok_or_else(|| {
            tracing::error!("Couldn't get probe as JtagAccess");
            ArmError::Dap(DapError::Protocol(WireProtocol::Jtag))
        })?;

        let mut this = Icepick { probe };

        // Reset the JTAG bus, which will remove all TAPs except the main ICEPICK.
        this.probe.tap_reset().map_err(ArmError::Probe)?;

        // If the default protocol is cJTAG, enable full JTAG mode
        if protocol == DefaultProtocol::CJtag {
            this.ctag_to_jtag()?;
        }

        // Get a listing of devices on the JTAG bus
        let tap_count = this
            .scan_jtag()
            .inspect_err(|e| tracing::error!("Unable to scan JTAG: {e}"))?;
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
        tracing::info!("Selecting target 0");
        this.probe
            .select_target(0)
            .inspect_err(|e| tracing::error!("Unable to select target 0: {e}"))?;

        // Enable write by setting the `ConnectKey` to 0b1001 (0x9) as per TRM section 6.3.3
        this.probe
            .write_register(IR_CONNECT, &[0x89], 8)
            .inspect_err(|e| tracing::error!("Couldn't write IR_CONNECT: {e}"))?;

        // Write to register 1 in the ICEPICK control block - keep JTAG powered in test logic reset
        this.icepick_router(IcepickRoutingRegister::Sysctrl, SYSCTRL_DEFAULT)?;

        Ok(this)
    }

    /// Print a list of IDCODEs on the JTAG bus.
    fn scan_jtag(&mut self) -> Result<u8, ArmError> {
        let mut tap_count = 0;
        tracing::trace!("Scan of JTAG bus:");
        // Enter DRSHIFT state.
        for tms in [
            true,  // DRSELECT
            false, // DRCAPTURE
            false, // DRSHIFT
        ] {
            self.raw_jtag_cycle(tms, false)?;
        }

        // Keep reading IDCODEs out until we get zeroes back.
        for index in 0..255 {
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
        for tms in [
            true,  // DRSHIFT
            true,  // DREXIT1
            false, // Run/Idle
        ] {
            self.raw_jtag_cycle(tms, false)?;
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
    pub(crate) fn select_tap(&mut self, secondary_tap: u8, tap_name: &str) -> Result<(), ArmError> {
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
            .set_expected_scan_chain(&[
                ScanChainElement {
                    name: Some(tap_name.to_owned()),
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

    /// Raw access to JTAG
    fn raw_jtag_cycle(&mut self, tms: bool, tdi: bool) -> Result<(), ArmError> {
        let mut data = BitVec::new();
        data.push(tdi);
        self.probe.shift_raw_sequence(JtagSequence {
            tdo_capture: false,
            tms,
            data,
        })?;
        Ok(())
    }

    /// This function implements a Zero Bit Scan(ZBS)
    ///
    /// The ZBS defined in section 6.2.2.1 of this document:
    /// <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
    ///
    /// This function assumes that the JTAG state machine is in the Run-Test/Idle state
    fn zero_bit_scan(&mut self) -> Result<(), ArmError> {
        for tms in [
            true,  // DRSELECT
            false, // DRCAPTURE
            true,  // DREXIT1
            false, // DRPAUSE
            true,  // DREXIT2
            true,  // DRUPDATE
            false, // Run/Idle
        ] {
            self.raw_jtag_cycle(tms, true)?;
        }
        Ok(())
    }

    /// Load a value into the IR or DR register
    ///
    /// This function moves through the JTAG state machine to load a value into
    /// the IR or DR register. The function assumes that the JTAG state machine is in
    /// either the Run-Test/Idle or Select-DR-Scan state.
    ///
    /// * `cycles`    - Number of TCK cycles to shift in the data to either IR or DR
    /// * `reg`       - The value to shift into either IR or DR
    /// * `action`    - Whether to load the IR or DR register, if IR is wanted then `JtagState::ShiftIR` should be passed
    ///   otherwise the default is to load DR.
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_reg(&mut self, cycles: u8, reg: u64, action: JtagOperation) -> Result<(), ArmError> {
        // DRSELECT
        self.raw_jtag_cycle(true, true)?;

        if action == JtagOperation::ShiftIr {
            // IRSELECT
            self.raw_jtag_cycle(true, true)?;
        }

        for tms in [
            false, // DR/IR CAPTURE
            true,  // EXIT1
            false, // PAUSE
            true,  // EXIT2
            false, // SHIFT
        ] {
            self.raw_jtag_cycle(tms, true)?;
        }

        // Shift out the bits
        for i in 0..cycles {
            // On the last cycle we want to leave the shift state
            let tms = i == cycles - 1;
            // Mask the register value to get the bit we want to shift in
            let reg_masked = (reg & (0x01 << u64::from(i))) != 0;
            // Send to the probe
            self.raw_jtag_cycle(tms, reg_masked)?;
        }

        // DR/IR UPDATE
        self.raw_jtag_cycle(true, true)?;
        // Run/Test Idle
        self.raw_jtag_cycle(false, true)?;

        Ok(())
    }

    /// Load a value into the IR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the IR register
    ///
    /// * `cycles`    - Number of TCK cycles to shift in the data to IR
    /// * `ir`        - The value to shift into either IR
    fn shift_ir(&mut self, ir: u64) -> Result<(), ArmError> {
        // This is a wrapper around shift_reg that loads the IR register
        self.shift_reg(IR_LEN_IN_BITS, ir, JtagOperation::ShiftIr)?;

        Ok(())
    }

    /// Load a value into the DR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the DR register
    ///
    /// * `cycles`    - Number of TCK cycles to shift in the data to DR
    /// * `reg`       - The value to shift into either DR
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_dr(&mut self, cycles: u8, reg: u64) -> Result<(), ArmError> {
        self.shift_reg(cycles, reg, JtagOperation::ShiftDr)?;
        Ok(())
    }

    /// Disable "Compact JTAG" support and enable full JTAG.
    pub(crate) fn ctag_to_jtag(&mut self) -> Result<(), ArmError> {
        // Load IR with BYPASS
        self.shift_ir(IR_BYPASS.into())?;

        // cJTAG: Open Command Window
        // This is described in section 6.2.2.1 of this document:
        // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // Also refer to the openocd implementation:
        // <https://github.com/openocd-org/openocd/blob/60d11a881fb2d1f34584ba975749feb6fc1c9d03/tcl/target/ti/cjtag.cfg#L6-L35>
        self.zero_bit_scan()?;
        self.zero_bit_scan()?;
        self.shift_dr(1, 0xff)?;

        // cJTAG: Switch to 4 pin
        // This is described in section 6.2.2.2 of this document:
        // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // Also refer to the openocd implementation:
        // <https://github.com/openocd-org/openocd/blob/60d11a881fb2d1f34584ba975749feb6fc1c9d03/tcl/target/ti/cjtag.cfg#L6-L35>
        self.shift_dr(2, 0xff)?;
        self.shift_dr(9, 0xff)?;

        // Load IR with BYPASS so that future state transitions don't affect IR
        self.shift_ir(IR_BYPASS.into())?;

        // Load IR with IDCODE to support scanning
        self.shift_ir(IR_IDCODE.into())?;

        Ok(())
    }
}
