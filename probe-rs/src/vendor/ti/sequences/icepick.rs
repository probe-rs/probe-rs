//! Controls for the ICEPICK JTAG mux used on some TI parts

use crate::architecture::arm::{ArmError, DapError, DapProbe};
use crate::probe::jtag::chain::JtagChain;
use crate::probe::{BitSequence, DebugProbeError, JtagBatch, TapState, WireProtocol};
use bitvec::field::BitField;
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
    probe: JtagChain<'a>,
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
        let chain = interface.try_as_jtag_chain().ok_or_else(|| {
            tracing::error!("Couldn't get probe as JtagChain");
            ArmError::Dap(DapError::Protocol(WireProtocol::Jtag))
        })?;

        let mut this = Icepick { probe: chain };

        {
            let mut batch = JtagBatch::new();
            this.probe.tap_reset(&mut batch);
            this.probe.run(batch).map_err(ArmError::Probe)?;
        }

        if protocol == DefaultProtocol::CJtag {
            this.ctag_to_jtag()?;
        }

        let tap_count = this
            .scan_jtag()
            .inspect_err(|e| tracing::error!("Unable to scan JTAG: {e}"))?;
        if tap_count == 0 {
            tracing::error!("No TAP devices found!");
            return Err(ArmError::Probe(DebugProbeError::TargetNotFound));
        }

        this.probe.set_chain(&[ScanChainElement {
            name: Some("ICEPICK".to_owned()),
            ir_len: Some(IR_LEN_IN_BITS),
        }]);
        tracing::info!("Selecting target 0");
        this.probe
            .select(0)
            .inspect_err(|e| tracing::error!("Unable to select target 0: {e}"))?;

        {
            let mut batch = JtagBatch::new();
            let ir = BitSequence::from_bytes(&IR_CONNECT.to_le_bytes(), IR_LEN_IN_BITS as usize);
            this.probe.shift_ir(&mut batch, &ir);
            let connect = BitSequence::from_bytes(&[0x89], 8);
            this.probe.exchange_dr(&mut batch, &connect);
            this.probe.run_test_idle(&mut batch, 0);
            this.probe
                .run(batch)
                .map_err(ArmError::Probe)
                .inspect_err(|e| tracing::error!("Couldn't write IR_CONNECT: {e}"))?;
        }

        this.icepick_router(IcepickRoutingRegister::Sysctrl, SYSCTRL_DEFAULT)?;

        Ok(this)
    }

    /// Print a list of IDCODEs on the JTAG bus.
    fn scan_jtag(&mut self) -> Result<u8, ArmError> {
        let mut tap_count = 0;
        tracing::trace!("Scan of JTAG bus:");

        for index in 0..255 {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftDr);
            let handle = batch.exchange(BitSequence::repeat(false, 32));
            batch.enter(TapState::RunTestIdle);
            let mut results = self.probe.run(batch).map_err(ArmError::Probe)?;
            let idcode_bits = results
                .take(handle)
                .map_err(|_| ArmError::Probe(DebugProbeError::Other("missing IDCODE".into())))?;
            let idcode = idcode_bits.as_bits().load_be::<u32>();

            tracing::trace!("    TAP index {index}: 0x{idcode:08x}");
            if idcode == 0 {
                break;
            }
            tap_count += 1;
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
        let rw = 1;
        let dr = (rw << 31) | (u32::from(register) << 24) | (payload & 0xFFFFFF);
        let dr_bits = BitSequence::from_bytes(&dr.to_le_bytes(), 32);
        let zero = BitSequence::from_bytes(&0u32.to_le_bytes(), 32);
        let ir = BitSequence::from_bytes(&IR_ROUTER.to_le_bytes(), IR_LEN_IN_BITS as usize);

        let mut batch = JtagBatch::new();
        self.probe.shift_ir(&mut batch, &ir);
        self.probe.exchange_dr(&mut batch, &dr_bits);
        self.probe.shift_ir(&mut batch, &ir);
        let handle = self.probe.exchange_dr(&mut batch, &zero);
        self.probe.run_test_idle(&mut batch, 0);
        let mut results = self.probe.run(batch).map_err(ArmError::Probe)?;
        let response = results
            .take(handle)
            .map_err(|_| ArmError::Probe(DebugProbeError::Other("missing router result".into())))?;
        tracing::trace!(
            "Value of {register:02x?}: 0x{:08x}",
            response.as_bits().load_le::<u32>()
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
        // Enter the bypass state to remove the ICEPick from the scan chain.
        // This will insert three cycles after the configuration in order to make
        // the target TAP appear.
        {
            let mut batch = JtagBatch::new();
            let ir = BitSequence::from_bytes(&IR_BYPASS.to_le_bytes(), IR_LEN_IN_BITS as usize);
            self.probe.shift_ir(&mut batch, &ir);
            self.probe
                .exchange_dr(&mut batch, &BitSequence::repeat(false, 1));
            self.probe.run_test_idle(&mut batch, 3);
            self.probe.run(batch).map_err(ArmError::Probe)?;
        }

        self.probe.set_expected(&[
            ScanChainElement {
                name: Some(tap_name.to_owned()),
                ir_len: Some(4),
            },
            ScanChainElement {
                name: Some("ICEPICK".to_owned()),
                ir_len: Some(IR_LEN_IN_BITS),
            },
        ]);

        tracing::trace!("Should be active now");
        self.scan_jtag()?;

        Ok(())
    }

    /// This function implements a Zero Bit Scan(ZBS)
    ///
    /// The ZBS defined in section 6.2.2.1 of this document:
    /// <https://www.ti.com/lit/ug/swcu185f/swcu185f.pdf>
    ///
    /// This function assumes that the JTAG state machine is in the Run-Test/Idle state
    fn zero_bit_scan(&mut self) -> Result<(), ArmError> {
        let mut batch = JtagBatch::new();
        batch.enter(TapState::PauseDr);
        batch.enter(TapState::RunTestIdle);
        self.probe.run(batch).map_err(ArmError::Probe)?;
        Ok(())
    }

    fn shift_ir_value(&mut self, ir: u32) -> Result<(), ArmError> {
        let mut batch = JtagBatch::new();
        let ir_bits = BitSequence::from_bytes(&ir.to_le_bytes(), IR_LEN_IN_BITS as usize);
        self.probe.shift_ir(&mut batch, &ir_bits);
        self.probe.run_test_idle(&mut batch, 0);
        self.probe.run(batch).map_err(ArmError::Probe)?;
        Ok(())
    }

    fn exchange_dr_value(&mut self, bits: BitSequence) -> Result<(), ArmError> {
        let mut batch = JtagBatch::new();
        self.probe.exchange_dr(&mut batch, &bits);
        self.probe.run_test_idle(&mut batch, 0);
        self.probe.run(batch).map_err(ArmError::Probe)?;
        Ok(())
    }

    /// Disable "Compact JTAG" support and enable full JTAG.
    pub(crate) fn ctag_to_jtag(&mut self) -> Result<(), ArmError> {
        self.shift_ir_value(IR_BYPASS)?;

        self.zero_bit_scan()?;
        self.zero_bit_scan()?;
        self.exchange_dr_value(BitSequence::from_u64(1, 0xff))?;

        self.exchange_dr_value(BitSequence::from_u64(2, 0xff))?;
        self.exchange_dr_value(BitSequence::from_u64(9, 0xff))?;

        self.shift_ir_value(IR_BYPASS)?;
        self.shift_ir_value(IR_IDCODE)?;

        Ok(())
    }
}
