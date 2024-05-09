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
        #[allow(non_camel_case_types)]
        #[allow(clippy::upper_case_acronyms)]
        pub struct CPUID(u32);
        0xD00, "CPUID",
        impl From;
        pub implementer, _: 31, 24;
        pub variant, _: 23, 20;
        pub partno, _: 15, 4;
        pub revision, _: 3, 0;
    }
}
