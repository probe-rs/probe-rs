#![allow(unused)] // TODO remove

use std::ops::Range;

pub mod instruction;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Register {
    Cpu(CpuRegister),
    Special(SpecialRegister),
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

impl CpuRegister {
    pub const fn scratch() -> Self {
        Self::A3
    }

    pub const fn address(self) -> u8 {
        self as u8
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

#[allow(non_upper_case_globals)] // Aliasses have same style as other register names
impl SpecialRegister {
    // Aliasses
    pub const MpuEnB: Self = Self::RAsid;
    pub const MpuCfg: Self = Self::DTlbCfg;
    pub const Depc: Self = Self::IBreakC0;
    pub const Interrupt: Self = Self::IntSet;

    pub const fn address(self) -> u8 {
        self as u8
    }
}

pub struct CacheConfig {
    pub line_size: u32,
    pub size: u32,
    pub way_count: u8,
    pub regions: Vec<Range<u32>>,
}

impl CacheConfig {
    /// Returns if the given address is covered by the cache.
    pub fn contains(&self, address: u32) -> bool {
        self.regions.iter().any(|r| r.contains(&address))
    }
}

pub struct ChipConfig {
    /// IRAM, IROM, SRAM, SROM
    pub icache: CacheConfig,

    /// DRAM, DROM, SRAM, SROM
    pub dcache: CacheConfig,
}

impl CacheConfig {
    pub const fn not_present() -> Self {
        Self {
            line_size: 0,
            size: 0,
            way_count: 1,
            regions: vec![],
        }
    }
}
