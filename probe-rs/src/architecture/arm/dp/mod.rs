#[macro_use]
mod register_generation;

use super::Register;
use bitfield::bitfield;
use jep106::JEP106Code;

pub trait DebugPort {
    fn version(&self) -> &'static str;
}

pub trait DPAccess<PORT, R>
where
    PORT: DebugPort,
    R: DPRegister<PORT>,
{
    type Error: std::fmt::Debug;
    fn read_dp_register(&mut self, port: &PORT) -> Result<R, Self::Error>;

    fn write_dp_register(&mut self, port: &PORT, register: R) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum DPBankSel {
    Unknown,
    DontCare,
    Bank(u8),
}

pub trait DPRegister<PortType: DebugPort>: Register {
    const DP_BANK: DPBankSel;
}

/// Debug PortType V1
pub struct DPv1 {}

impl DebugPort for DPv1 {
    fn version(&self) -> &'static str {
        "DPv1"
    }
}

/// Debug PortType V2
pub struct DPv2 {}

impl DebugPort for DPv2 {
    fn version(&self) -> &'static str {
        "DPv2"
    }
}

bitfield! {
    #[derive(Clone)]
    pub struct Abort(u32);
    impl Debug;
    pub _, set_orunerrclr: 5;
    pub _, set_wderrclr: 4;
    pub _, set_stkerrclr: 3;
    pub _, set_stkcmpclr: 2;
    pub _, set_dapabort: 1;
}

impl From<u32> for Abort {
    fn from(raw: u32) -> Self {
        Abort(raw)
    }
}

impl From<Abort> for u32 {
    fn from(raw: Abort) -> Self {
        raw.0
    }
}

impl DPRegister<DPv1> for Abort {
    const DP_BANK: DPBankSel = DPBankSel::DontCare;
}

impl Register for Abort {
    const ADDRESS: u8 = 0x0;
    const NAME: &'static str = "ABORT";
}

bitfield! {
    #[derive(Clone)]
    pub struct Ctrl(u32);
    impl Debug;
    pub csyspwrupack, _: 31;
    pub csyspwrupreq, set_csyspwrupreq: 30;
    pub cdbgpwrupack, _: 29;
    pub cdbgpwrupreq, set_cdbgpwrupreq: 28;
    pub cdbgrstack, _: 27;
    pub c_dbg_rst_req, set_c_dbg_rst_req: 26;
    pub u16, trn_cnt, set_trn_cnt: 23, 12;
    pub u8, mask_lane, set_mask_lane: 11, 8;
    pub w_data_err, _ : 7;
    pub read_ok, _ : 6;
    pub sticky_err, _: 5;
    pub stick_cmp, _: 4;
    pub u8, trn_mode, _: 3, 2;
    pub sticky_orun, _: 1;
    pub orun_detect, set_orun_detect: 0;
}

impl Default for Ctrl {
    fn default() -> Self {
        Ctrl(0)
    }
}

impl From<u32> for Ctrl {
    fn from(raw: u32) -> Self {
        Ctrl(raw)
    }
}

impl From<Ctrl> for u32 {
    fn from(raw: Ctrl) -> Self {
        raw.0
    }
}

impl DPRegister<DPv1> for Ctrl {
    const DP_BANK: DPBankSel = DPBankSel::Bank(0);
}

impl Register for Ctrl {
    const ADDRESS: u8 = 0x4;
    const NAME: &'static str = "CTRL/STAT";
}

bitfield! {
    #[derive(Clone)]
    pub struct Select(u32);
    impl Debug;
    pub u8, ap_sel, set_ap_sel: 31, 24;
    pub u8, ap_bank_sel, set_ap_bank_sel: 7, 4;
    pub u8, dp_bank_sel, set_dp_bank_sel: 3, 0;
}

impl From<u32> for Select {
    fn from(raw: u32) -> Self {
        Select(raw)
    }
}

impl From<Select> for u32 {
    fn from(raw: Select) -> Self {
        raw.0
    }
}

impl DPRegister<DPv1> for Select {
    const DP_BANK: DPBankSel = DPBankSel::DontCare;
}

impl Register for Select {
    const ADDRESS: u8 = 0x8;
    const NAME: &'static str = "SELECT";
}

bitfield! {
    #[derive(Clone)]
    pub struct DPIDR(u32);
    impl Debug;
    pub u8, revision, _: 31, 28;
    pub u8, part_no, _: 27, 20;
    pub min, _: 16;
    pub u8, version, _: 15, 12;
    pub designer, _: 11, 1;
    u8, jep_cc, _: 11, 8;
    u8, jep_id, _: 7, 1;
}

impl From<u32> for DPIDR {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<DPIDR> for u32 {
    fn from(raw: DPIDR) -> Self {
        raw.0
    }
}

impl DPRegister<DPv1> for DPIDR {
    const DP_BANK: DPBankSel = DPBankSel::DontCare;
}

impl Register for DPIDR {
    const ADDRESS: u8 = 0x0;
    const NAME: &'static str = "DPIDR";
}

#[derive(Debug)]
pub struct DebugPortId {
    pub revision: u8,
    pub part_no: u8,
    pub version: DebugPortVersion,
    pub min_dp_support: MinDpSupport,
    pub designer: JEP106Code,
}

impl From<DPIDR> for DebugPortId {
    fn from(dpidr: DPIDR) -> DebugPortId {
        DebugPortId {
            revision: dpidr.revision(),
            part_no: dpidr.part_no(),
            version: dpidr.version().into(),
            min_dp_support: dpidr.min().into(),
            designer: JEP106Code::new(dpidr.jep_cc(), dpidr.jep_id()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MinDpSupport {
    NotImplemented,
    Implemented,
}

impl From<bool> for MinDpSupport {
    fn from(bit_set: bool) -> Self {
        if bit_set {
            MinDpSupport::Implemented
        } else {
            MinDpSupport::NotImplemented
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum DebugPortVersion {
    DPv0,
    DPv1,
    DPv2,
    Unsupported,
}

impl From<u8> for DebugPortVersion {
    fn from(value: u8) -> Self {
        match value {
            0 => DebugPortVersion::DPv0,
            1 => DebugPortVersion::DPv1,
            2 => DebugPortVersion::DPv2,
            _ => DebugPortVersion::Unsupported,
        }
    }
}
