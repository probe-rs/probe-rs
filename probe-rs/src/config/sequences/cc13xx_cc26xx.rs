//! Sequences for cc13xx_cc26xx devices

use std::sync::Arc;

use super::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::communication_interface::DapProbe;
use crate::architecture::arm::ArmError;
use crate::architecture::arm::sequences::ArmProbe;

/// Marker struct indicating initialization sequencing for cc13xx_cc26xx family parts.
pub struct CC13xxCC26xx {}

// IR register values, see https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf table 6-7
const IR_ROUTER: u64 = 0x02;
const IR_CONNECT: u64 = 0x07;
const IR_BYPASS: u64 = 0x3F;
const IR_LEN_IN_BITS: u8 = 6;

#[derive(PartialEq)]
enum JtagState {
    RunTestIdle = 0x1,
    SelectDRScan = 0x2,
    ShiftDR = 0x03,
    ShiftIR = 0x04,
}

// Set the bottom n bits of a u64 to 1
// This is lifted directly from:
// https://users.rust-lang.org/t/how-to-make-an-integer-with-n-bits-set-without-overflow/63078/6
fn set_n_bits(x: u32) -> u64 {
    u64::checked_shl(1, x).unwrap_or(0).wrapping_sub(1)
}

impl CC13xxCC26xx {
    /// Create the sequencer for the cc13xx_cc26xx family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// This function implements a Zero Bit Scan(ZBS)
    ///
    /// The ZBS defined in section 6.2.2.1 of this document:
    /// https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf
    ///
    /// This function assumes that the JTAG state machine is in the Run-Test/Idle state
    ///
    /// * `interface` - Reference to interface to interact with CmsisDap
    fn zero_bit_scan(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // Enter DRSELECT state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRCAPTURE state
        interface.jtag_sequence(1, false, 0x01)?;
        // Enter DREXIT1 state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRPAUSE state
        interface.jtag_sequence(1, false, 0x01)?;
        // Enter DREXIT2 state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRUPDATE state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter Run/Idle state
        interface.jtag_sequence(1, false, 0x01)?;

        Ok(())
    }
    /// Load a value into the IR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the IR register
    ///
    /// * `interface` - Reference to interface to interact with CmsisDap
    /// * `cycles`    - Number of TCK cycles to shift in the data to IR
    /// * `reg`       - The value to shift into either IR
    /// * `state`     - The current state of the JTAG state machine. Note this will be updated by this function so that the
    ///                state is correct after the function returns
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_ir(
        &self,
        interface: &mut dyn DapProbe,
        ir: u64,
        state: &mut JtagState,
        end_state: JtagState,
    ) -> Result<(), ArmError> {
        // THis is a wrapper around shift_reg that loads the IR register
        self.shift_reg(
            interface,
            IR_LEN_IN_BITS,
            ir,
            state,
            JtagState::ShiftIR,
            end_state,
        )?;

        Ok(())
    }

    /// Load a value into the DR register
    ///
    /// This function is a wrapper on `shift_reg` that loads a value into the DR register
    ///
    /// * `interface` - Reference to interface to interact with CmsisDap
    /// * `cycles`    - Number of TCK cycles to shift in the data to DR
    /// * `reg`       - The value to shift into either DR
    /// * `state`     - The current state of the JTAG state machine. Note this will be updated by this function so that the
    ///                state is correct after the function returns
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_dr(
        &self,
        interface: &mut dyn DapProbe,
        cycles: u8,
        reg: u64,
        state: &mut JtagState,
        end_state: JtagState,
    ) -> Result<(), ArmError> {
        self.shift_reg(interface, cycles, reg, state, JtagState::ShiftDR, end_state)?;
        Ok(())
    }
    /// Load a value into the IR or DR register
    ///
    /// This function moves through the JTAG state machine to load a value into
    /// the IR or DR register. The function assumes that the JTAG state machine is in
    /// either the Run-Test/Idle or Select-DR-Scan state.
    ///
    /// * `interface` - Reference to interface to interact with CmsisDap
    /// * `cycles`    - Number of TCK cycles to shift in the data to either IR or DR
    /// * `reg`       - The value to shift into either IR or DR
    /// * `state`     - The current state of the JTAG state machine. Note this will be updated by this function so that the
    ///                 state is correct after the function returns
    /// * `action`    - Whether to load the IR or DR register, if IR is wanted then `JtagState::ShiftIR` should be passed
    ///                 otherwise the default is to load DR.
    /// * `end_state` - The state to end in, this can either be `JtagState::RunTestIdle` or `JtagState::SelectDRScan`
    fn shift_reg(
        &self,
        interface: &mut dyn DapProbe,
        cycles: u8,
        reg: u64,
        state: &mut JtagState,
        action: JtagState,
        end_state: JtagState,
    ) -> Result<(), ArmError> {
        if *state == JtagState::RunTestIdle {
            // Enter DR-Scan state
            interface.jtag_sequence(1, true, 0x01)?;
        }
        if action == JtagState::ShiftIR {
            // Enter IR-Scan state,
            interface.jtag_sequence(1, true, 0x01)?;
        }
        // Enter DR or IR CAPTURE state
        interface.jtag_sequence(1, false, 0x01)?;
        // Enter DR or IR EXIT1 state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter DRor IR PAUSE state
        interface.jtag_sequence(1, false, 0x01)?;
        // Enter DR or IR EXIT2 state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter DR or IR SHIFT state
        interface.jtag_sequence(1, false, 0x01)?;
        for i in 0..cycles {
            // On the last cycle we want to leave the shift state
            let tms = i == cycles - 1;
            // Mask the register value to get the bit we want to shift in
            let reg_masked = (reg & (0x01 << u64::from(i))) != 0;
            // Send to the probe
            interface.jtag_sequence(1, tms, u64::from(reg_masked))?;
        }
        // Enter DR or IR UPDATE state
        interface.jtag_sequence(1, true, 0x01)?;
        // Enter either run-test-idle or select-dr-scan depending on the end state
        interface.jtag_sequence(1, end_state == JtagState::SelectDRScan, 0x01)?;

        // Update the state to the desired end state
        *state = end_state;

        Ok(())
    }

    /// Reads or writes a given register using the ICEPICK router
    ///
    /// This function is used to load the router register of the ICEPICK TAP
    /// and connect a given data register to the TDO.
    ///
    /// This is a direct port from the openocd implementation:
    /// https://github.com/openocd-org/openocd/blob/master/tcl/target/icepick.cfg#L56-L70
    ///
    /// * `interface` - Reference to interface to interact with CmsisDap
    /// * `rw`        - 0 for read, 1 for write
    /// * `block`     - The register block to access
    /// * `register`  - The register to access
    /// * `payload`   - The data to write to the register
    /// * `state`     - The current state of the JTAG state machine. Note this will be updated by this function
    ///                so that the state is correct after the function returns
    fn icepick_router(
        &self,
        interface: &mut dyn DapProbe,
        rw: u32,
        block: u32,
        register: u32,
        payload: u32,
        state: &mut JtagState,
    ) -> Result<(), ArmError> {
        // Build the DR value based on the requested operation. The DR value
        // is based on the input arguments and contains several bitfields
        let dr: u32 = ((rw & 0x1) << 31)
            | ((block & 0x7) << 28)
            | ((register & 0xF) << 24)
            | (payload & 0xFFFFFF);

        // Load IR with the router instruction, this allows us to write or read a data register
        self.shift_ir(interface, IR_ROUTER, state, JtagState::SelectDRScan)?;
        // Load the data register with the register block, address, and data
        // The address and value to be written is encoded in the DR value
        self.shift_dr(interface, 32, dr as u64, state, JtagState::SelectDRScan)?;
        Ok(())
    }

    /// Does setup of the ICEPICK
    ///
    /// This will setup the ICEPICK to have the CPU/DAP on the scan chain and
    /// also power and enable the debug interface for use with probe-rs
    ///
    /// This is a direct port of the openocd implementation:
    /// https://github.com/openocd-org/openocd/blob/master/tcl/target/icepick.cfg#L81-L124
    /// A few things were removed to fit the cc13xx_cc26xx family.
    fn enable_icepick(
        &self,
        interface: &mut dyn DapProbe,
        state: &mut JtagState,
    ) -> Result<(), ArmError> {
        let port = 0;
        let block = 0x02;

        // Select the Connect register
        self.shift_ir(interface, IR_CONNECT, state, JtagState::SelectDRScan)?;
        // Enable write, set the `ConnectKey` to 0b1001 (0x9) as per TRM section 6.3.3
        self.shift_dr(interface, 8, 0x89, state, JtagState::SelectDRScan)?;
        // Write to register 1 in the ICEPICK control block - keep JTAG powered in test logic reset
        self.icepick_router(interface, 1, 0, 1, 0x000080, state)?;
        // Write to register 0 in the Debug TAP linking block (Section 6.3.4.3)
        // Namely:
        // * [20]   : `InhibitSleep`
        // * [16:14]: `ResetControl == Wait In Reset`
        // * [8]    : `SelectTAP == 1`
        // * [3]    : `ForceActive == Enable clocks`
        self.icepick_router(interface, 1, block, port, 0x110108, state)?;

        // Enter the bypass state
        self.shift_ir(interface, IR_BYPASS, state, JtagState::RunTestIdle)?;

        // Remain in run-test idle for 10 cycles
        interface.jtag_sequence(10, false, set_n_bits(10))?;

        Ok(())
    }
}

impl ArmDebugSequence for CC13xxCC26xx {

    fn reset_system(
        &self,
        _interface: &mut dyn ArmProbe,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {

        // The only reset that is supported is system reset, which will reset the entire chip
        // losing the JTAG configuration (back in two pin JTAG mode)
        // TODO! Find a way to reset setup JTAG again after reset
        Ok(())
    }

    fn debug_port_setup(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // Ensure current debug interface is in reset state.
        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF)?;

        match interface.active_protocol() {
            Some(crate::WireProtocol::Jtag) => {
                let mut jtag_state: JtagState = JtagState::RunTestIdle;
                // Enter Run-Test-Idle state
                interface.jtag_sequence(1, false, 0x00)?;
                // Load IR with BYPASS
                self.shift_ir(
                    interface,
                    IR_BYPASS,
                    &mut jtag_state,
                    JtagState::RunTestIdle,
                )?;

                // cJTAG: Open Command Window
                // This is described in section 6.2.2.1 of this document:
                // https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf
                // Also refer to the openocd implementation:
                // https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35
                self.zero_bit_scan(interface)?;
                self.zero_bit_scan(interface)?;
                self.shift_dr(interface, 1, 0x01, &mut jtag_state, JtagState::RunTestIdle)?;

                // cJTAG: Switch to 4 pin
                // This is described in section 6.2.2.2 of this document:
                // https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf
                // Also refer to the openocd implementation:
                // https://github.com/openocd-org/openocd/blob/master/tcl/target/ti-cjtag.cfg#L6-L35
                self.shift_dr(
                    interface,
                    2,
                    set_n_bits(2),
                    &mut jtag_state,
                    JtagState::RunTestIdle,
                )?;
                self.shift_dr(
                    interface,
                    9,
                    set_n_bits(9),
                    &mut jtag_state,
                    JtagState::RunTestIdle,
                )?;

                // Load IR with BYPASS so that future state transitions don't affect IR
                self.shift_ir(
                    interface,
                    IR_BYPASS,
                    &mut jtag_state,
                    JtagState::RunTestIdle,
                )?;

                // Connect CPU DAP to top level TAP
                // This is done by interacting with the top level TAP which is called ICEPICK
                // Some resouces on the ICEPICK, note that the cc13xx_cc26xx family implements ICEPICK-C
                // https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf, Section 6.3
                // https://software-dl.ti.com/ccs/esd/documents/xdsdebugprobes/emu_icepick.html
                self.enable_icepick(interface, &mut jtag_state)?;

                // Call the configure JTAG function. We don't derive the scan chain at runtime
                // for these devices, but regardless the scan chain must be told to the debug probe
                // We avoid the live scan for the following reasons:
                // 1. Only the ICEPICK is connected at boot so we need to manually the CPU to the scan chain
                // 2. Entering test logic reset disconects the CPU again
                interface.configure_jtag()?;
            }
            Some(crate::WireProtocol::Swd) => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "The cc13xx_cc26xx family doesn't support SWD".into(),
                )
                .into());
            }
            _ => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        Ok(())
    }
}
