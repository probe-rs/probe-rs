//! Sequences for ARMv6 STM32s: STM32F0, STM32G0, STM32L0.
//!
//! This covers devices where DBGMCU is at 0x40015800 and has the DBG_STANDBY and DBG_STOP bits.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress, armv6m::Dhcsr,
        memory::ArmMemoryInterface, sequences::ArmDebugSequence,
    },
};

/// Supported families for custom sequences on ARMv6 STM32 devices.
#[derive(Debug)]
pub enum Stm32Armv6Family {
    /// STM32F0 family
    F0,

    /// STM32L0 family
    L0,

    /// STM32G0 family
    G0,
}

/// Marker structure for ARMv6 STM32 devices.
#[derive(Debug)]
pub struct Stm32Armv6 {
    family: Stm32Armv6Family,
}

impl Stm32Armv6 {
    /// Create the sequencer for ARMv6 STM32 devices.
    pub fn create(family: Stm32Armv6Family) -> Arc<Self> {
        Arc::new(Self { family })
    }
}

mod rcc {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the RCC peripheral
    const RCC: u64 = 0x40021000;

    macro_rules! enable_reg {
        ($name:ident, $offset:literal, $bit:literal) => {
            bitfield! {
                pub struct $name(u32);
                impl Debug;
                pub u8, dbgen, enable_dbg: $bit;
            }
            impl $name {
                const ADDRESS: u64 = $offset;
                /// Read the enable register from memory.
                pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
                    let contents = memory.read_word_32(RCC + Self::ADDRESS)?;
                    Ok(Self(contents))
                }

                /// Write the enable register to memory.
                pub fn write(
                    &mut self,
                    memory: &mut dyn ArmMemoryInterface,
                ) -> Result<(), ArmError> {
                    memory.write_word_32(RCC + Self::ADDRESS, self.0)
                }
            }
        };
    }

    // Create enable registers for each device family.
    // On F0 and L0 this is bit 22 in APB2ENR, while on G0 it's bit 27 in APBENR1.
    enable_reg!(EnrF0, 0x18, 22);
    enable_reg!(EnrL0, 0x34, 22);
    enable_reg!(EnrG0, 0x3c, 27);
}

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0x40015800;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0360: STM32F0
        /// family reference manual" section 26.9.3.
        pub struct Control(pub u32);
        impl Debug;

        pub u8, dbg_standby, enable_standby_debug: 2;
        pub u8, dbg_stop, enable_stop_debug: 1;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

        /// Read the control register from memory.
        pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

impl Stm32Armv6 {
    fn dbgen(&self, memory: &mut dyn ArmMemoryInterface) -> Result<bool, ArmError> {
        Ok(match self.family {
            Stm32Armv6Family::F0 => rcc::EnrF0::read(memory)?.dbgen(),
            Stm32Armv6Family::L0 => rcc::EnrL0::read(memory)?.dbgen(),
            Stm32Armv6Family::G0 => rcc::EnrG0::read(memory)?.dbgen(),
        })
    }

    /// Sets or clears DBGEN and the stop and standby debug bits. The RCC register is shared with
    /// the firmware: a clock it enables between our read and write would be switched off again, so
    /// a running core is halted around the read-modify-writes.
    fn set_debug(&self, memory: &mut dyn ArmMemoryInterface, on: bool) -> Result<(), ArmError> {
        let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
        let running = !dhcsr.s_halt();
        if running {
            let mut halt = Dhcsr(0);
            halt.enable_write();
            halt.set_c_debugen(true);
            halt.set_c_halt(true);
            memory.write_word_32(Dhcsr::get_mmio_address(), halt.into())?;
            let start = Instant::now();
            while !Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                if start.elapsed() > Duration::from_millis(100) {
                    tracing::warn!("Core did not halt, changing DBGMCU while it runs");
                    break;
                }
            }
        }

        match self.family {
            Stm32Armv6Family::F0 => {
                let mut enr = rcc::EnrF0::read(memory)?;
                enr.enable_dbg(on);
                enr.write(memory)?;
            }
            Stm32Armv6Family::L0 => {
                let mut enr = rcc::EnrL0::read(memory)?;
                enr.enable_dbg(on);
                enr.write(memory)?;
            }
            Stm32Armv6Family::G0 => {
                let mut enr = rcc::EnrG0::read(memory)?;
                enr.enable_dbg(on);
                enr.write(memory)?;
            }
        }
        let mut cr = dbgmcu::Control::read(memory)?;
        cr.enable_standby_debug(on);
        cr.enable_stop_debug(on);
        cr.write(memory)?;

        if running {
            let mut resume = Dhcsr(0);
            resume.enable_write();
            resume.set_c_debugen(dhcsr.c_debugen());
            memory.write_word_32(Dhcsr::get_mmio_address(), resume.into())?;
        }
        Ok(())
    }
}

impl ArmDebugSequence for Stm32Armv6 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        if self.dbgen(&mut *memory)? {
            let cr = dbgmcu::Control::read(&mut *memory)?;
            if cr.dbg_standby() && cr.dbg_stop() {
                return Ok(());
            }
        }
        self.set_debug(&mut *memory, true)
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
    ) -> Result<(), ArmError> {
        // Without DBGEN, DBGMCU is not clocked: there is nothing to switch off.
        if !self.dbgen(memory)? {
            return Ok(());
        }
        self.set_debug(memory, false)
    }
}
