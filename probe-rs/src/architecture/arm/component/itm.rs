//! Module for using the ITM.
//!
//! ITM = Instrumentation Trace Macrocell

use super::super::memory::romtable::Component;
use super::DebugRegister;
use crate::{Core, Error};

pub const _ITM_PID: [u8; 8] = [0x1, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];

pub struct Itm<'probe: 'core, 'core> {
    component: &'core Component,
    core: &'core mut Core<'probe>,
}

const _REGISTER_OFFSET_ITM_TPR: u32 = 0xE40;
const REGISTER_OFFSET_ITM_TCR: u32 = 0xE80;
const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

impl<'probe: 'core, 'core> Itm<'probe, 'core> {
    pub fn new(core: &'core mut Core<'probe>, component: &'core Component) -> Self {
        Itm { component, core }
    }

    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    pub fn tx_enable(&mut self) -> Result<(), Error> {
        let mut value = self
            .component
            .read_reg(self.core, REGISTER_OFFSET_ITM_TCR)?;

        value |= 1 << 0; // ITMENA: enable ITM (master switch)
        value |= 1 << 1; // TSENA: enable local timestamps
        value |= 1 << 2; // SYNENA: Enable sync pulses, note DWT_CTRL.SYNCTAP must be configured.
        value |= 1 << 3; // TXENA: forward DWT packets to ITM
        value |= 1 << 11; // GTSFREQ: generate global timestamp every 8192 cycles
        value |= 13 << 16; // 7 bits trace bus ID
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ITM_TCR, value)?;

        // Enable all 32 channels.
        self.component.write_reg(
            self.core,
            register::ITM_TER::ADDRESS,
            register::ITM_TER::enable_all().into(),
        )?;

        Ok(())
    }
}

mod register {
    use super::super::DebugRegister;

    bitfield::bitfield! {
        #[allow(non_camel_case_types)]
        #[derive(Copy, Clone)]
        pub struct ITM_TER(u32);
        impl Debug;
        pub stim31, set_stim31: 31;
        pub stim30, set_stim30: 30;
        pub stim29, set_stim29: 29;
        pub stim28, set_stim28: 28;
        pub stim27, set_stim27: 27;
        pub stim26, set_stim26: 26;
        pub stim25, set_stim25: 25;
        pub stim24, set_stim24: 24;
        pub stim23, set_stim23: 23;
        pub stim22, set_stim22: 22;
        pub stim21, set_stim21: 21;
        pub stim20, set_stim20: 20;
        pub stim19, set_stim19: 19;
        pub stim18, set_stim18: 18;
        pub stim17, set_stim17: 17;
        pub stim16, set_stim16: 16;
        pub stim15, set_stim15: 15;
        pub stim14, set_stim14: 14;
        pub stim13, set_stim13: 13;
        pub stim12, set_stim12: 12;
        pub stim11, set_stim11: 11;
        pub stim10, set_stim10: 10;
        pub stim09, set_stim09: 9;
        pub stim08, set_stim08: 8;
        pub stim07, set_stim07: 7;
        pub stim06, set_stim06: 6;
        pub stim05, set_stim05: 5;
        pub stim04, set_stim04: 4;
        pub stim03, set_stim03: 3;
        pub stim02, set_stim02: 2;
        pub stim01, set_stim01: 1;
        pub stim00, set_stim00: 0;
    }

    impl ITM_TER {
        pub fn enable_all() -> Self {
            Self(0xFFFF_FFFF)
        }

        pub fn disable_all() -> Self {
            Self(0x0000_0000)
        }
    }

    impl From<u32> for ITM_TER {
        fn from(value: u32) -> Self {
            Self(value)
        }
    }

    impl From<ITM_TER> for u32 {
        fn from(value: ITM_TER) -> Self {
            value.0
        }
    }

    impl DebugRegister for ITM_TER {
        const ADDRESS: u32 = 0xE00;
        const NAME: &'static str = "ITM_TER";
    }
}
