//! Module for using the SCS.
//!
//! SCS = System Control Space

use self::register::CPUID;

use super::super::memory::romtable::CoresightComponent;
use super::DebugRegister;
use crate::architecture::arm::{ArmError, ArmProbeInterface};

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
            .read_reg(self.interface, CPUID::ADDRESS)
            .map(CPUID)
    }
}

mod register {
    use super::super::DebugRegister;

    bitfield::bitfield! {
        /// B3.2.3 CPUID Base Register
        #[allow(non_camel_case_types)]
        #[derive(Copy, Clone)]
        pub struct CPUID(u32);
        impl Debug;
        pub implementer, _: 31, 24;
        pub variant, _: 23, 20;
        pub partno, _: 15, 4;
        pub revision, _: 3, 0;
    }

    impl From<u32> for CPUID {
        fn from(value: u32) -> Self {
            Self(value)
        }
    }

    impl From<CPUID> for u32 {
        fn from(value: CPUID) -> Self {
            value.0
        }
    }

    impl DebugRegister for CPUID {
        const ADDRESS: u32 = 0xD00;
        const NAME: &'static str = "CPUID";
    }
}
