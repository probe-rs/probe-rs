//! Module for using the SCS.
//!
//! SCS = System Control Space

use self::register::CPUID;

use super::super::memory::romtable::CoresightComponent;
use crate::{
    architecture::arm::{ArmError, ArmProbeInterface},
    MemoryMappedRegister,
};

/// An interface to control the SCS (System Control Space) of a MCU.
pub struct Scs<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> Scs<'a> {
    /// Create a new SCS interface from a probe and a ROM table component.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Scs {
            interface,
            component,
        }
    }

    /// B3.2.3 CPUID Base Register
    pub fn cpuid(&mut self) -> Result<CPUID, ArmError> {
        self.component
            .read_reg(self.interface, CPUID::ADDRESS_OFFSET as u32)
            .map(CPUID)
    }
}

mod register {
    use crate::memory_mapped_bitfield_register;

    memory_mapped_bitfield_register! {
        /// B3.2.3 CPUID Base Register
        pub struct CPUID(u32);
        0xD00, "CPUID",
        impl From;
        pub implementer, _: 31, 24;
        pub variant, _: 23, 20;
        pub partno, _: 15, 4;
        pub revision, _: 3, 0;
    }

    impl CPUID {
        /// Implementer code.
        pub fn implementer_name(&self) -> String {
            match self.implementer() {
                0x41 => String::from("ARM Ltd"),
                0x49 => String::from("Infineon"),
                0x72 => String::from("Realtek"),
                other => format!("{other:#x}"),
            }
        }

        pub fn part_name(&self) -> String {
            match self.implementer() {
                0x41 => match self.partno() {
                    0xC20 => String::from("Cortex-M0"),
                    0xC21 => String::from("Cortex-M1"),
                    0xC23 => String::from("Cortex-M3"),
                    0xC24 => String::from("Cortex-M4"),
                    0xC27 => String::from("Cortex-M7"),
                    0xC60 => String::from("Cortex-M0+"),
                    0xD20 => String::from("Cortex-M23"),
                    0xD21 => String::from("Cortex-M33"),
                    0xD31 => String::from("Cortex-M35P"),
                    0xD22 => String::from("Cortex-M55"),
                    0xD23 => String::from("Cortex-M85"),
                    0xD24 => String::from("Cortex-M52"),
                    _ => format!("{:#x}", self.partno()),
                },
                _ => format!("{:#x}", self.partno()),
            }
        }
    }
}
