use crate::{RegisterId, architecture::xtensa::communication_interface::XtensaError};

pub mod instruction;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Register {
    Cpu(CpuRegister),
    Special(SpecialRegister),

    /// Program counter. The physical register depends on the debug level.
    CurrentPc,

    /// Program state. The physical register depends on the debug level.
    CurrentPs,
}

impl Register {
    pub(crate) fn is_cpu_register(self) -> bool {
        matches!(self, Register::Cpu(_))
    }
}

impl TryFrom<RegisterId> for Register {
    type Error = XtensaError;

    fn try_from(value: RegisterId) -> Result<Self, Self::Error> {
        match value.0.to_le_bytes() {
            [id, 0] => Ok(Self::Cpu(CpuRegister::try_from(id)?)),
            [id, 1] => Ok(Self::Special(SpecialRegister::try_from(id)?)),
            [0, 0xFF] => Ok(Self::CurrentPc),
            [1, 0xFF] => Ok(Self::CurrentPs),
            _ => Err(XtensaError::RegisterNotAvailable),
        }
    }
}

impl From<Register> for RegisterId {
    fn from(register: Register) -> RegisterId {
        match register {
            Register::Cpu(reg) => reg.into(),
            Register::Special(reg) => reg.into(),
            Register::CurrentPc => RegisterId(u16::from_le_bytes([0, 0xFF])),
            Register::CurrentPs => RegisterId(u16::from_le_bytes([1, 0xFF])),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CpuRegister {
    A0 = 0,
    A1 = 1,
    A2 = 2,
    A3 = 3,
    A4 = 4,
    A5 = 5,
    A6 = 6,
    A7 = 7,
    A8 = 8,
    A9 = 9,
    A10 = 10,
    A11 = 11,
    A12 = 12,
    A13 = 13,
    A14 = 14,
    A15 = 15,
}

impl TryFrom<u8> for CpuRegister {
    type Error = XtensaError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::A0),
            1 => Ok(Self::A1),
            2 => Ok(Self::A2),
            3 => Ok(Self::A3),
            4 => Ok(Self::A4),
            5 => Ok(Self::A5),
            6 => Ok(Self::A6),
            7 => Ok(Self::A7),
            8 => Ok(Self::A8),
            9 => Ok(Self::A9),
            10 => Ok(Self::A10),
            11 => Ok(Self::A11),
            12 => Ok(Self::A12),
            13 => Ok(Self::A13),
            14 => Ok(Self::A14),
            15 => Ok(Self::A15),
            _ => Err(XtensaError::RegisterNotAvailable),
        }
    }
}

impl From<CpuRegister> for RegisterId {
    fn from(register: CpuRegister) -> RegisterId {
        RegisterId(u16::from_le_bytes([register as u8, 0]))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SpecialRegister {
    Lbeg = 0,
    Lend = 1,
    Lcount = 2,
    Sar = 3,
    Br = 4,
    Litbase = 5,
    Scompare1 = 12,
    AccLo = 16,
    AccHi = 17,
    M0 = 32,
    M1 = 33,
    M2 = 34,
    M3 = 35,
    Windowbase = 72,
    Windowstart = 73,
    PteVAddr = 83,
    RAsid = 90,
    // MpuEnB = 90,
    ITlbCfg = 91,
    DTlbCfg = 92,
    // MpuCfg = 92,
    ERAccess = 95,
    IBreakEnable = 96,
    Memctl = 97,
    CacheAdrDis = 98,
    AtomCtl = 99,
    Ddr = 104,
    Mepc = 106,
    Meps = 107,
    Mesave = 108,
    Mesr = 109,
    Mecr = 110,
    MeVAddr = 111,
    IBreakA0 = 128,
    IBreakA1 = 129,
    DBreakA0 = 144,
    DBreakA1 = 145,
    DBreakC0 = 160,
    DBreakC1 = 161,
    Epc1 = 177,
    Epc2 = 178,
    Epc3 = 179,
    Epc4 = 180,
    Epc5 = 181,
    Epc6 = 182,
    Epc7 = 183,
    IBreakC0 = 192,
    IBreakC1 = 193,
    // Depc = 192,
    Eps2 = 194,
    Eps3 = 195,
    Eps4 = 196,
    Eps5 = 197,
    Eps6 = 198,
    Eps7 = 199,
    ExcSave1 = 209,
    ExcSave2 = 210,
    ExcSave3 = 211,
    ExcSave4 = 212,
    ExcSave5 = 213,
    ExcSave6 = 214,
    ExcSave7 = 215,
    CpEnable = 224,
    // Interrupt = 226,
    IntSet = 226,
    IntClear = 227,
    IntEnable = 228,
    Ps = 230,
    VecBase = 231,
    ExcCause = 232,
    DebugCause = 233,
    CCount = 234,
    Prid = 235,
    ICount = 236,
    ICountLevel = 237,
    ExcVaddr = 238,
    CCompare0 = 240,
    CCompare1 = 241,
    CCompare2 = 242,
    Misc0 = 244,
    Misc1 = 245,
    Misc2 = 246,
    Misc3 = 247,
}

impl TryFrom<u8> for SpecialRegister {
    type Error = XtensaError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            v if v == Self::Lbeg as u8 => Ok(Self::Lbeg),
            v if v == Self::Lend as u8 => Ok(Self::Lend),
            v if v == Self::Lcount as u8 => Ok(Self::Lcount),
            v if v == Self::Sar as u8 => Ok(Self::Sar),
            v if v == Self::Br as u8 => Ok(Self::Br),
            v if v == Self::Litbase as u8 => Ok(Self::Litbase),
            v if v == Self::Scompare1 as u8 => Ok(Self::Scompare1),
            v if v == Self::AccLo as u8 => Ok(Self::AccLo),
            v if v == Self::AccHi as u8 => Ok(Self::AccHi),
            v if v == Self::M0 as u8 => Ok(Self::M0),
            v if v == Self::M1 as u8 => Ok(Self::M1),
            v if v == Self::M2 as u8 => Ok(Self::M2),
            v if v == Self::M3 as u8 => Ok(Self::M3),
            v if v == Self::Windowbase as u8 => Ok(Self::Windowbase),
            v if v == Self::Windowstart as u8 => Ok(Self::Windowstart),
            v if v == Self::PteVAddr as u8 => Ok(Self::PteVAddr),
            v if v == Self::RAsid as u8 => Ok(Self::RAsid),
            v if v == Self::ITlbCfg as u8 => Ok(Self::ITlbCfg),
            v if v == Self::DTlbCfg as u8 => Ok(Self::DTlbCfg),
            v if v == Self::ERAccess as u8 => Ok(Self::ERAccess),
            v if v == Self::IBreakEnable as u8 => Ok(Self::IBreakEnable),
            v if v == Self::Memctl as u8 => Ok(Self::Memctl),
            v if v == Self::CacheAdrDis as u8 => Ok(Self::CacheAdrDis),
            v if v == Self::AtomCtl as u8 => Ok(Self::AtomCtl),
            v if v == Self::Ddr as u8 => Ok(Self::Ddr),
            v if v == Self::Mepc as u8 => Ok(Self::Mepc),
            v if v == Self::Meps as u8 => Ok(Self::Meps),
            v if v == Self::Mesave as u8 => Ok(Self::Mesave),
            v if v == Self::Mesr as u8 => Ok(Self::Mesr),
            v if v == Self::Mecr as u8 => Ok(Self::Mecr),
            v if v == Self::MeVAddr as u8 => Ok(Self::MeVAddr),
            v if v == Self::IBreakA0 as u8 => Ok(Self::IBreakA0),
            v if v == Self::IBreakA1 as u8 => Ok(Self::IBreakA1),
            v if v == Self::DBreakA0 as u8 => Ok(Self::DBreakA0),
            v if v == Self::DBreakA1 as u8 => Ok(Self::DBreakA1),
            v if v == Self::DBreakC0 as u8 => Ok(Self::DBreakC0),
            v if v == Self::DBreakC1 as u8 => Ok(Self::DBreakC1),
            v if v == Self::Epc1 as u8 => Ok(Self::Epc1),
            v if v == Self::Epc2 as u8 => Ok(Self::Epc2),
            v if v == Self::Epc3 as u8 => Ok(Self::Epc3),
            v if v == Self::Epc4 as u8 => Ok(Self::Epc4),
            v if v == Self::Epc5 as u8 => Ok(Self::Epc5),
            v if v == Self::Epc6 as u8 => Ok(Self::Epc6),
            v if v == Self::Epc7 as u8 => Ok(Self::Epc7),
            v if v == Self::IBreakC0 as u8 => Ok(Self::IBreakC0),
            v if v == Self::IBreakC1 as u8 => Ok(Self::IBreakC1),
            v if v == Self::Eps2 as u8 => Ok(Self::Eps2),
            v if v == Self::Eps3 as u8 => Ok(Self::Eps3),
            v if v == Self::Eps4 as u8 => Ok(Self::Eps4),
            v if v == Self::Eps5 as u8 => Ok(Self::Eps5),
            v if v == Self::Eps6 as u8 => Ok(Self::Eps6),
            v if v == Self::Eps7 as u8 => Ok(Self::Eps7),
            v if v == Self::ExcSave1 as u8 => Ok(Self::ExcSave1),
            v if v == Self::ExcSave2 as u8 => Ok(Self::ExcSave2),
            v if v == Self::ExcSave3 as u8 => Ok(Self::ExcSave3),
            v if v == Self::ExcSave4 as u8 => Ok(Self::ExcSave4),
            v if v == Self::ExcSave5 as u8 => Ok(Self::ExcSave5),
            v if v == Self::ExcSave6 as u8 => Ok(Self::ExcSave6),
            v if v == Self::ExcSave7 as u8 => Ok(Self::ExcSave7),
            v if v == Self::CpEnable as u8 => Ok(Self::CpEnable),
            v if v == Self::IntSet as u8 => Ok(Self::IntSet),
            v if v == Self::IntClear as u8 => Ok(Self::IntClear),
            v if v == Self::IntEnable as u8 => Ok(Self::IntEnable),
            v if v == Self::Ps as u8 => Ok(Self::Ps),
            v if v == Self::VecBase as u8 => Ok(Self::VecBase),
            v if v == Self::ExcCause as u8 => Ok(Self::ExcCause),
            v if v == Self::DebugCause as u8 => Ok(Self::DebugCause),
            v if v == Self::CCount as u8 => Ok(Self::CCount),
            v if v == Self::Prid as u8 => Ok(Self::Prid),
            v if v == Self::ICount as u8 => Ok(Self::ICount),
            v if v == Self::ICountLevel as u8 => Ok(Self::ICountLevel),
            v if v == Self::ExcVaddr as u8 => Ok(Self::ExcVaddr),
            v if v == Self::CCompare0 as u8 => Ok(Self::CCompare0),
            v if v == Self::CCompare1 as u8 => Ok(Self::CCompare1),
            v if v == Self::CCompare2 as u8 => Ok(Self::CCompare2),
            v if v == Self::Misc0 as u8 => Ok(Self::Misc0),
            v if v == Self::Misc1 as u8 => Ok(Self::Misc1),
            v if v == Self::Misc2 as u8 => Ok(Self::Misc2),
            v if v == Self::Misc3 as u8 => Ok(Self::Misc3),
            _ => Err(XtensaError::RegisterNotAvailable),
        }
    }
}

impl From<SpecialRegister> for RegisterId {
    fn from(register: SpecialRegister) -> RegisterId {
        RegisterId(u16::from_le_bytes([register as u8, 1]))
    }
}

#[expect(non_upper_case_globals)] // Aliasses have same style as other register names
impl SpecialRegister {
    // Aliasses
    pub const MpuEnB: Self = Self::RAsid;
    pub const MpuCfg: Self = Self::DTlbCfg;
    pub const Depc: Self = Self::IBreakC0;
    pub const Interrupt: Self = Self::IntSet;
}

impl From<CpuRegister> for Register {
    fn from(value: CpuRegister) -> Self {
        Self::Cpu(value)
    }
}

impl From<SpecialRegister> for Register {
    fn from(value: SpecialRegister) -> Self {
        Self::Special(value)
    }
}
