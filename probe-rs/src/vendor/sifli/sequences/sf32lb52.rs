use crate::MemoryMappedRegister;
use crate::architecture::arm::ArmError;
use crate::architecture::arm::armv8m::Dhcsr;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use probe_rs_target::CoreType;
use std::sync::Arc;

#[derive(Debug)]
pub struct Sf32lb52 {}

impl Sf32lb52 {
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

mod pmuc {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the PMUC component
    const PMUC: u64 = 0x500C_A000;

    bitfield! {
        /// The control register (CR) of the PMUC.
        pub struct Control(u32);
        impl Debug;

        pub u8, pin1_sel, set_pin1_sel: 19, 15;
        pub u8, pin0_sel, set_pin0_sel: 14, 10;
        pub u8, pin1_mode, set_pin1_mode: 9, 7;
        pub u8, pin0_mode, set_pin0_mode: 6, 4;
        pub bool, pin_ret, set_pin_ret: 3;
        pub bool, reboot, set_reboot: 2;
        pub bool, hiber_en, set_hiber_en: 1;
        pub bool, sel_lpclk, set_sel_lpclk: 0;
    }

    impl Control {
        /// The offset of the Control register in the PMUC block.
        const ADDRESS: u64 = 0x00;

        /// Read the control register from memory.
        pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(PMUC + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(PMUC + Self::ADDRESS, self.0)
        }
    }
}

fn halt_core(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let mut value = Dhcsr(0);
    value.set_c_halt(true);
    value.set_c_debugen(true);
    value.enable_write();

    interface.write_word_32(Dhcsr::get_mmio_address(), value.into())?;
    Ok(())
}

impl ArmDebugSequence for Sf32lb52 {
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv8m::Aircr;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        let _ = interface.write_word_32(Aircr::get_mmio_address(), aircr.into());

        std::thread::sleep(std::time::Duration::from_millis(500));
        interface.update_core_status(crate::CoreStatus::Unknown);

        Ok(())
    }
}
