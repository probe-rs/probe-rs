//! CMSIS-DAP implementation of the AVR UPDI interface.

use super::CmsisDap;
use super::{
    AvrChipDescriptor, AvrDebugState, AvrMemoryRegion, debug_avr_cleanup, debug_avr_enter,
    debug_avr_halt, debug_avr_hw_break_clear, debug_avr_hw_break_set, debug_avr_read_memory,
    debug_avr_read_pc, debug_avr_read_registers, debug_avr_read_sp, debug_avr_read_sreg,
    debug_avr_reset, debug_avr_run, debug_avr_status, debug_avr_step, erase_attached_pkobn_updi,
    read_attached_pkobn_updi_region, write_attached_pkobn_updi_flash,
};
use crate::architecture::avr::UpdiInterface;
use crate::probe::DebugProbeError;

/// CMSIS-DAP UPDI interface for AVR debug and programming operations.
///
/// Wraps a borrowed `CmsisDap` probe together with the chip descriptor and
/// debug session state. Implements [`UpdiInterface`] by delegating to the
/// existing `debug_avr_*` free functions.
pub struct CmsisDapUpdi<'a> {
    probe: &'a mut CmsisDap,
    chip: &'a AvrChipDescriptor,
    debug_state: &'a mut AvrDebugState,
}

impl<'a> CmsisDapUpdi<'a> {
    /// Create a new CMSIS-DAP UPDI interface.
    pub fn new(
        probe: &'a mut CmsisDap,
        chip: &'a AvrChipDescriptor,
        debug_state: &'a mut AvrDebugState,
    ) -> Self {
        Self {
            probe,
            chip,
            debug_state,
        }
    }
}

impl UpdiInterface for CmsisDapUpdi<'_> {
    fn enter_debug_mode(&mut self) -> Result<(), DebugProbeError> {
        debug_avr_enter(self.probe, self.chip, self.debug_state)
    }

    fn halt(&mut self) -> Result<u32, DebugProbeError> {
        debug_avr_halt(self.probe, self.chip, self.debug_state)
    }

    fn run(&mut self) -> Result<(), DebugProbeError> {
        debug_avr_run(self.probe, self.chip, self.debug_state)
    }

    fn step(&mut self) -> Result<u32, DebugProbeError> {
        debug_avr_step(self.probe, self.chip, self.debug_state)
    }

    fn read_pc(&mut self) -> Result<u32, DebugProbeError> {
        debug_avr_read_pc(self.probe, self.chip, self.debug_state)
    }

    fn status(&mut self) -> Result<bool, DebugProbeError> {
        debug_avr_status(self.probe, self.chip, self.debug_state)
    }

    fn read_registers(&mut self) -> Result<[u8; 32], DebugProbeError> {
        debug_avr_read_registers(self.probe, self.chip, self.debug_state)
    }

    fn read_sreg(&mut self) -> Result<u8, DebugProbeError> {
        debug_avr_read_sreg(self.probe, self.chip, self.debug_state)
    }

    fn read_sp(&mut self) -> Result<u16, DebugProbeError> {
        debug_avr_read_sp(self.probe, self.chip, self.debug_state)
    }

    fn hw_break_set(&mut self, bp_index: u8, address: u32) -> Result<(), DebugProbeError> {
        debug_avr_hw_break_set(self.probe, self.chip, self.debug_state, bp_index, address)
    }

    fn hw_break_clear(&mut self, bp_index: u8) -> Result<(), DebugProbeError> {
        debug_avr_hw_break_clear(self.probe, self.chip, self.debug_state, bp_index)
    }

    fn reset(&mut self) -> Result<(), DebugProbeError> {
        debug_avr_reset(self.probe, self.chip, self.debug_state)
    }

    fn read_memory(
        &mut self,
        memtype: u8,
        address: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        debug_avr_read_memory(
            self.probe,
            self.chip,
            self.debug_state,
            memtype,
            address,
            length,
        )
    }

    fn cleanup(&mut self) -> Result<(), DebugProbeError> {
        debug_avr_cleanup(self.probe, self.chip, self.debug_state)
    }

    fn read_region(
        &mut self,
        region: AvrMemoryRegion,
        offset: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        read_attached_pkobn_updi_region(self.probe, self.chip, region, offset, length)
    }

    fn write_flash(&mut self, offset: u32, data: &[u8]) -> Result<(), DebugProbeError> {
        write_attached_pkobn_updi_flash(self.probe, self.chip, offset, data)
    }

    fn erase_chip(&mut self) -> Result<(), DebugProbeError> {
        erase_attached_pkobn_updi(self.probe, self.chip)
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        use crate::probe::DebugProbe;
        self.probe.target_reset()
    }

    fn debug_state(&self) -> &AvrDebugState {
        self.debug_state
    }

    fn debug_state_mut(&mut self) -> &mut AvrDebugState {
        self.debug_state
    }

    fn chip(&self) -> &AvrChipDescriptor {
        self.chip
    }
}

/// Owned CMSIS-DAP UPDI interface for session-level storage.
///
/// Unlike [`CmsisDapUpdi`] which borrows the probe, this variant owns the
/// `Probe` instance. Created by consuming a `Probe` during session
/// attachment, following the same pattern as ARM's `Box<dyn ArmDebugInterface>`.
pub struct OwnedCmsisDapUpdi {
    probe: crate::probe::Probe,
    chip: AvrChipDescriptor,
    debug_state: AvrDebugState,
}

impl OwnedCmsisDapUpdi {
    /// Create an owned UPDI interface from a `Probe`.
    ///
    /// The Probe must be a CMSIS-DAP probe; returns an error otherwise.
    pub fn from_probe(
        mut probe: crate::probe::Probe,
        chip: AvrChipDescriptor,
    ) -> Result<Self, crate::Error> {
        // Verify it's a CMSIS-DAP probe
        if crate::probe::Probe::try_into::<CmsisDap>(&mut probe).is_none() {
            return Err(crate::Error::NotImplemented(
                "AVR requires a CMSIS-DAP probe",
            ));
        }
        Ok(Self {
            probe,
            chip,
            debug_state: AvrDebugState::default(),
        })
    }

    /// Get split borrows of (probe, chip, debug_state).
    /// Panics if the probe is not CmsisDap (checked at construction).
    fn parts(&mut self) -> (&mut CmsisDap, &AvrChipDescriptor, &mut AvrDebugState) {
        let cmsis = crate::probe::Probe::try_into::<CmsisDap>(&mut self.probe)
            .expect("OwnedCmsisDapUpdi: probe must be CmsisDap");
        (cmsis, &self.chip, &mut self.debug_state)
    }
}

impl UpdiInterface for OwnedCmsisDapUpdi {
    fn enter_debug_mode(&mut self) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_enter(p, c, s)
    }

    fn halt(&mut self) -> Result<u32, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_halt(p, c, s)
    }

    fn run(&mut self) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_run(p, c, s)
    }

    fn step(&mut self) -> Result<u32, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_step(p, c, s)
    }

    fn read_pc(&mut self) -> Result<u32, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_read_pc(p, c, s)
    }

    fn status(&mut self) -> Result<bool, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_status(p, c, s)
    }

    fn read_registers(&mut self) -> Result<[u8; 32], DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_read_registers(p, c, s)
    }

    fn read_sreg(&mut self) -> Result<u8, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_read_sreg(p, c, s)
    }

    fn read_sp(&mut self) -> Result<u16, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_read_sp(p, c, s)
    }

    fn hw_break_set(&mut self, bp_index: u8, address: u32) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_hw_break_set(p, c, s, bp_index, address)
    }

    fn hw_break_clear(&mut self, bp_index: u8) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_hw_break_clear(p, c, s, bp_index)
    }

    fn reset(&mut self) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_reset(p, c, s)
    }

    fn read_memory(
        &mut self,
        memtype: u8,
        address: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_read_memory(p, c, s, memtype, address, length)
    }

    fn cleanup(&mut self) -> Result<(), DebugProbeError> {
        let (p, c, s) = self.parts();
        debug_avr_cleanup(p, c, s)
    }

    fn read_region(
        &mut self,
        region: AvrMemoryRegion,
        offset: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        let (p, c, _s) = self.parts();
        read_attached_pkobn_updi_region(p, c, region, offset, length)
    }

    fn write_flash(&mut self, offset: u32, data: &[u8]) -> Result<(), DebugProbeError> {
        let (p, c, _s) = self.parts();
        write_attached_pkobn_updi_flash(p, c, offset, data)
    }

    fn erase_chip(&mut self) -> Result<(), DebugProbeError> {
        let (p, c, _s) = self.parts();
        erase_attached_pkobn_updi(p, c)
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        use crate::probe::DebugProbe;
        self.parts().0.target_reset()
    }

    fn debug_state(&self) -> &AvrDebugState {
        &self.debug_state
    }

    fn debug_state_mut(&mut self) -> &mut AvrDebugState {
        &mut self.debug_state
    }

    fn chip(&self) -> &AvrChipDescriptor {
        &self.chip
    }
}
