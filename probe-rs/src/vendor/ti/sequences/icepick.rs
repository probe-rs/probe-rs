//! Sequences for cc13xx_cc26xx devices

use crate::architecture::arm::ArmError;
use crate::architecture::arm::communication_interface::DapProbe;

/// A TI ICEPick device. An ICEPick manages a JTAG device and can be used to add or
/// remove JTAG TAPs from a bus.
#[derive(Debug)]
pub struct Icepick<'a> {
    interface: &'a mut dyn DapProbe,
    jtag_state: JtagState,
}

// IR register values, see <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf> table 6-7
const IR_ROUTER: u64 = 0x02;
const IR_CONNECT: u64 = 0x07;
const IR_BYPASS: u64 = 0x3F;
const IR_LEN_IN_BITS: u8 = 6;

/// Write to register 0 in the Debug TAP linking block (Section 6.3.4.3)
/// Namely:
/// * [20]   : `InhibitSleep`
/// * [16:14]: `ResetControl == Reset`
/// * [8]    : `SelectTAP == 1`
/// * [3]    : `ForceActive == Enable clocks`
const SD_TAP_DEFAULT: u32 = (1 << 20) | (1 << 8) | (1 << 3);
const SD_TAP_WAIT_IN_RESET: u32 = 1 << 14;
const SD_TAP_RELEASE_FROM_WIR: u32 = 1 << 17;

const SYSCTRL_DEFAULT: u32 = 0x80;
const SYSCTRL_RESET: u32 = 1;

#[derive(PartialEq, Debug)]
enum JtagState {
    RunTestIdle = 0x1,
    SelectDrScan = 0x2,
}

#[derive(PartialEq)]
enum JtagOperation {
    ShiftDr = 0x03,
    ShiftIr = 0x04,
}

#[repr(u32)]
enum IcepickRoutingRegister {
    /// Control the ICEPick itself
    Sysctrl = 1,
    /// Modify parameters of a specific Secondary Tap
    SdTap(u8),
}

impl From<IcepickRoutingRegister> for u32 {
    fn from(value: IcepickRoutingRegister) -> Self {
        match value {
            IcepickRoutingRegister::Sysctrl => 1u32,
            IcepickRoutingRegister::SdTap(tap) => 0b010_0000 | tap as u32,
        }
    }
}

// Set the bottom n bits of a u64 to 1
// This is lifted directly from:
// <https://users.rust-lang.org/t/how-to-make-an-integer-with-n-bits-set-without-overflow/63078/6>
fn set_n_bits(x: u32) -> u64 {
    u64::checked_shl(1, x).unwrap_or(0).wrapping_sub(1)
}

impl<'a> Icepick<'a> {
    /// Create a new ICEPick interface. An ICEPick is a mux that sits on the JTAG bus
    /// and must be asked to enable various parts on the bus in order to allow us to
    /// talk to them. By default, the ICEPick will disable all secondary TAPs.
    pub fn new(interface: &'a mut dyn DapProbe) -> Result<Self, ArmError> {
        // Put the interface in Test-Logic Reset
        for _ in 0..5 {
            interface.jtag_sequence(1, true, 0)?;
        }
        // Move to Run-Test/Idle
        interface.jtag_sequence(1, false, 0)?;

        Ok(Icepick {
            interface,
            jtag_state: JtagState::RunTestIdle,
        })
    }

    /// Creates a new ICEPick device that is assumed to be initialized already.
    pub fn initialized(interface: &'a mut dyn DapProbe) -> Result<Self, ArmError> {
        Ok(Icepick {
            interface,
            jtag_state: JtagState::RunTestIdle,
        })
    }

    /// Indicate we want to catch a reet by setting the `RESETCONTROL` to `Wait-in-reset`
    pub fn catch_reset(&mut self, secondary_tap: u8) -> Result<(), ArmError> {
        self.icepick_router(
            IcepickRoutingRegister::SdTap(secondary_tap),
            SD_TAP_DEFAULT | SD_TAP_WAIT_IN_RESET,
        )
    }

    /// After a sysreset, the core will be waiting in a reset state.
    /// This will release the target from reset by setting the `RELEASEFROMWIR` bit.
    pub fn release_from_reset(&mut self, secondary_tap: u8) -> Result<(), ArmError> {
        self.icepick_router(
            IcepickRoutingRegister::SdTap(secondary_tap),
            SD_TAP_DEFAULT | SD_TAP_RELEASE_FROM_WIR,
        )
    }

    /// This function implements a Zero Bit Scan(ZBS)
    ///
    /// The ZBS defined in section 6.2.2.1 of this document:
    /// <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
    ///
    /// This function assumes that the JTAG state machine is in the Run-Test/Idle state
    fn zero_bit_scan(&mut self) -> Result<(), ArmError> {
        // Enter DRSELECT state
        self.interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRCAPTURE state
        self.interface.jtag_sequence(1, false, 0x01)?;
        // Enter DREXIT1 state
        self.interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRPAUSE state
        self.interface.jtag_sequence(1, false, 0x01)?;
        // Enter DREXIT2 state
        self.interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRUPDATE state
        self.interface.jtag_sequence(1, true, 0x01)?;
        // Enter Run/Idle state
        self.interface.jtag_sequence(1, false, 0x01)?;

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
    fn shift_reg(
        &mut self,
        cycles: u8,
        reg: u64,
        action: JtagOperation,
        end_state: JtagState,
    ) -> Result<(), ArmError> {
        if self.jtag_state == JtagState::RunTestIdle {
            // Enter DR-SCAN state
            self.interface.swj_sequence(1, 1)?;
        }

        if action == JtagOperation::ShiftIr {
            // Enter IR-SCAN state
            self.interface.swj_sequence(1, 1)?;
        }

        // Enter IR/DR CAPTURE -> EXIT1 -> PAUSE -> EXIT2 -> SHIFT state
        self.interface.swj_sequence(5, 0b01010)?;

        // Shift out the bits
        for i in 0..cycles {
            // On the last cycle we want to leave the shift state
            let tms = i == cycles - 1;
            // Mask the register value to get the bit we want to shift in
            let reg_masked = (reg & (0x01 << u64::from(i))) != 0;
            // Send to the probe
            self.interface
                .jtag_sequence(1, tms, u64::from(reg_masked))?;
        }

        // Enter DR/IR UPDATE -> RUN/TEST-IDLE or DR-SCAN state
        self.interface.swj_sequence(
            2,
            if end_state == JtagState::SelectDrScan {
                0b11
            } else {
                0b01
            },
        )?;

        // Update the state to the desired end state
        self.jtag_state = end_state;

        Ok(())
    }

    /// Load a value into the IR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the IR register
    ///
    /// * `cycles`    - Number of TCK cycles to shift in the data to IR
    /// * `ir`        - The value to shift into either IR
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_ir(&mut self, ir: u64, end_state: JtagState) -> Result<(), ArmError> {
        // This is a wrapper around shift_reg that loads the IR register
        self.shift_reg(IR_LEN_IN_BITS, ir, JtagOperation::ShiftIr, end_state)?;

        Ok(())
    }

    /// Load a value into the DR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the DR register
    ///
    /// * `cycles`    - Number of TCK cycles to shift in the data to DR
    /// * `reg`       - The value to shift into either DR
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_dr(&mut self, cycles: u8, reg: u64, end_state: JtagState) -> Result<(), ArmError> {
        self.shift_reg(cycles, reg, JtagOperation::ShiftDr, end_state)?;
        Ok(())
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

        // Load IR with the router instruction, this allows us to write or read a data register
        self.shift_ir(IR_ROUTER, JtagState::SelectDrScan)?;
        // Load the data register with the register block, address, and data
        // The address and value to be written is encoded in the DR value
        self.shift_dr(32, dr as u64, JtagState::SelectDrScan)?;
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
        tracing::trace!("Selecting seconary tap {secondary_tap}");
        // Select the Connect register
        self.shift_ir(IR_CONNECT, JtagState::SelectDrScan)?;
        // Enable write, set the `ConnectKey` to 0b1001 (0x9) as per TRM section 6.3.3
        self.shift_dr(8, 0x89, JtagState::SelectDrScan)?;
        // Write to register 1 in the ICEPICK control block - keep JTAG powered in test logic reset
        self.icepick_router(IcepickRoutingRegister::Sysctrl, SYSCTRL_DEFAULT)?;
        self.icepick_router(IcepickRoutingRegister::SdTap(secondary_tap), SD_TAP_DEFAULT)?;
        // Enter the bypass state to remove the ICEPick from the scan chain
        self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)?;

        // Remain in run-test idle for at least three cycles to activate the device
        self.interface.jtag_sequence(10, false, set_n_bits(10))?;

        Ok(())
    }

    pub(crate) fn sysreset(&mut self) -> Result<(), ArmError> {
        // Write to register 1 in the ICEPICK control block - keep JTAG powered in test logic reset.
        // Add bit 1 to initiate a reset.
        self.icepick_router(
            IcepickRoutingRegister::Sysctrl,
            SYSCTRL_DEFAULT | SYSCTRL_RESET,
        )
    }

    /// Disable "Compact JTAG" support and enable full JTAG.
    pub(crate) fn ctag_to_jtag(&mut self) -> Result<(), ArmError> {
        // Load IR with BYPASS
        self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)?;

        // cJTAG: Open Command Window
        // This is described in section 6.2.2.1 of this document:
        // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // Also refer to the openocd implementation:
        // <https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35>
        self.zero_bit_scan()?;
        self.zero_bit_scan()?;
        self.shift_dr(1, 0x01, JtagState::RunTestIdle)?;

        // cJTAG: Switch to 4 pin
        // This is described in section 6.2.2.2 of this document:
        // <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
        // Also refer to the openocd implementation:
        // <https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35>
        self.shift_dr(2, set_n_bits(2), JtagState::RunTestIdle)?;
        self.shift_dr(9, set_n_bits(9), JtagState::RunTestIdle)?;

        // Load IR with BYPASS so that future state transitions don't affect IR
        self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)?;

        Ok(())
    }

    /// Load IR with BYPASS so that future state transitions don't affect IR
    pub(crate) fn bypass(&mut self) -> Result<(), ArmError> {
        self.shift_ir(IR_BYPASS, JtagState::RunTestIdle)
    }
}
