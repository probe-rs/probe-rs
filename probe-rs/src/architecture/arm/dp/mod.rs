#[macro_use]
mod register_generation;

use super::{DapAccess, DpAddress, Register};
use bitfield::bitfield;
use jep106::JEP106Code;

use crate::DebugProbeError;
use std::fmt::Display;

#[derive(thiserror::Error, Debug)]
pub enum DebugPortError {
    #[error("Register {register} not supported by debug port version {version}")]
    UnsupportedRegister {
        register: &'static str,
        version: DebugPortVersion,
    },
    #[error("A Debug Probe Error occured")]
    DebugProbe(#[from] DebugProbeError),
}

impl From<DebugPortError> for DebugProbeError {
    fn from(error: DebugPortError) -> Self {
        DebugProbeError::ArchitectureSpecific(Box::new(error))
    }
}

pub trait DpAccess {
    fn read_dp_register<R: DpRegister>(&mut self, dp: DpAddress) -> Result<R, DebugPortError>;

    fn write_dp_register<R: DpRegister>(
        &mut self,
        dp: DpAddress,
        register: R,
    ) -> Result<(), DebugPortError>;
}

impl<T: DapAccess> DpAccess for T {
    fn read_dp_register<R: DpRegister>(&mut self, dp: DpAddress) -> Result<R, DebugPortError> {
        log::debug!("Reading DP register {}", R::NAME);
        let result = self.read_raw_dp_register(dp, R::ADDRESS)?;
        log::debug!("Read    DP register {}, value=0x{:08x}", R::NAME, result);
        Ok(result.into())
    }

    fn write_dp_register<R: DpRegister>(
        &mut self,
        dp: DpAddress,
        register: R,
    ) -> Result<(), DebugPortError> {
        let value = register.into();
        log::debug!("Writing DP register {}, value=0x{:08x}", R::NAME, value);
        self.write_raw_dp_register(dp, R::ADDRESS, value)?;
        Ok(())
    }
}

pub trait DpRegister: Register {
    const VERSION: DebugPortVersion;
}

bitfield! {
    #[derive(Clone)]
    pub struct Abort(u32);
    impl Debug;
    pub _, set_orunerrclr: 4;
    pub _, set_wderrclr: 3;
    pub _, set_stkerrclr: 2;
    pub _, set_stkcmpclr: 1;
    pub _, set_dapabort: 0;
}

impl Default for Abort {
    fn default() -> Self {
        Abort(0)
    }
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

impl DpRegister for Abort {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv1;
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

impl DpRegister for Ctrl {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv1;
}

impl Register for Ctrl {
    const ADDRESS: u8 = 0x04;
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

impl DpRegister for Select {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv1;
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
    pub u8, jep_cc, _: 11, 8;
    pub u8, jep_id, _: 7, 1;
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

impl DpRegister for DPIDR {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv1;
}

impl Register for DPIDR {
    const ADDRESS: u8 = 0x0;
    const NAME: &'static str = "DPIDR";
}

bitfield! {
    #[derive(Clone)]
    pub struct TARGETID(u32);
    impl Debug;
    pub u8, trevision, _: 31, 28;
    pub u16, tpartno, _: 27, 12;
    pub u16, tdesigner, _: 11, 1;
}

impl From<u32> for TARGETID {
    fn from(raw: u32) -> Self {
        Self(raw)
    }
}

impl From<TARGETID> for u32 {
    fn from(raw: TARGETID) -> Self {
        raw.0
    }
}

impl DpRegister for TARGETID {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv2;
}

impl Register for TARGETID {
    const ADDRESS: u8 = 0x24;
    const NAME: &'static str = "TARGETID";
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

#[derive(Debug, Clone)]
pub struct RdBuff(pub u32);

impl Register for RdBuff {
    const ADDRESS: u8 = 0xc;
    const NAME: &'static str = "RDBUFF";
}

impl From<u32> for RdBuff {
    fn from(val: u32) -> Self {
        RdBuff(val)
    }
}

impl From<RdBuff> for u32 {
    fn from(register: RdBuff) -> Self {
        let RdBuff(val) = register;
        val
    }
}

impl DpRegister for RdBuff {
    const VERSION: DebugPortVersion = DebugPortVersion::DPv1;
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

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum DebugPortVersion {
    DPv0,
    DPv1,
    DPv2,
    Unsupported(u8),
}

impl From<DebugPortVersion> for u8 {
    fn from(version: DebugPortVersion) -> Self {
        use DebugPortVersion::*;

        match version {
            DPv0 => 0,
            DPv1 => 1,
            DPv2 => 2,
            Unsupported(val) => val,
        }
    }
}

impl PartialOrd for DebugPortVersion {
    fn partial_cmp(&self, other: &DebugPortVersion) -> Option<std::cmp::Ordering> {
        let self_value = u8::from(*self);
        let other_value = u8::from(*other);

        self_value.partial_cmp(&other_value)
    }
}

impl Display for DebugPortVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        use DebugPortVersion::*;

        match self {
            DPv0 => write!(f, "DPv0"),
            DPv1 => write!(f, "DPv1"),
            DPv2 => write!(f, "DPv2"),
            Unsupported(version) => write!(f, "<unsupported Debugport Version {}>", version),
        }
    }
}

impl From<u8> for DebugPortVersion {
    fn from(value: u8) -> Self {
        match value {
            0 => DebugPortVersion::DPv0,
            1 => DebugPortVersion::DPv1,
            2 => DebugPortVersion::DPv2,
            value => DebugPortVersion::Unsupported(value),
        }
    }
}
