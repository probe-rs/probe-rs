pub mod instruction;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Register {
    Cpu(CpuRegister),
    Special(SpecialRegister),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CpuRegister {
    A3 = 3,
}

impl CpuRegister {
    pub const fn scratch() -> Self {
        Self::A3
    }

    pub const fn address(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SpecialRegister {
    Ddr = 104,
    ExcCause = 232,
    DebugCause = 233,
    ExcVaddr = 238,
}

impl SpecialRegister {
    pub const fn address(self) -> u8 {
        self as u8
    }
}
