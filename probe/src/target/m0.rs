use super::CoreRegister;
use bitfield::bitfield;

bitfield!{
    #[derive(Copy, Clone)]
    pub struct Dhcsr(u32);
    impl Debug;
    pub S_RESET_ST, _: 25;
    pub S_RETIRE_ST, _: 24;
    pub S_LOCKUP, _: 19;
    pub S_SLEEP, _: 18;
    pub S_HALT, _: 17;
    pub S_REGRDY, _: 16;
    pub _, set_C_MASKINTS: 3;
    pub _, set_C_STEP: 2;
    pub _, set_C_HALT: 1;
    pub _, set_C_DEBUGEN: 0;
}

impl From<u32> for Dhcsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dhcsr> for u32 {
    fn from(value: Dhcsr) -> Self {
        value.0
    }
}

impl CoreRegister for Dhcsr {
    const ADDRESS: u32 = 0xe000_edf0;
    const NAME: &'static str = "DHCSR";
}