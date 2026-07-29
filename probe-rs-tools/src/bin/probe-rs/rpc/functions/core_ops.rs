//! Wire-format mirrors of [`probe_rs::Core`] types.

use postcard_schema::Schema;
use probe_rs::CoreInformation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Copy, PartialEq, Eq)]
pub struct WireCoreInformation {
    pub pc: u64,
}

impl From<CoreInformation> for WireCoreInformation {
    fn from(value: CoreInformation) -> Self {
        Self { pc: value.pc }
    }
}

impl From<WireCoreInformation> for CoreInformation {
    fn from(value: WireCoreInformation) -> Self {
        Self { pc: value.pc }
    }
}

#[derive(Debug, Serialize, Deserialize, Schema, Clone, Copy, PartialEq, Eq)]
pub enum WireCoreType {
    Armv6m,
    Armv7a,
    Armv7r,
    Armv7m,
    Armv7em,
    Armv8a,
    Armv8m,
    Riscv,
    Riscv64,
    Xtensa,
}

impl From<probe_rs::CoreType> for WireCoreType {
    fn from(value: probe_rs::CoreType) -> Self {
        match value {
            probe_rs::CoreType::Armv6m => WireCoreType::Armv6m,
            probe_rs::CoreType::Armv7a => WireCoreType::Armv7a,
            probe_rs::CoreType::Armv7r => WireCoreType::Armv7r,
            probe_rs::CoreType::Armv7m => WireCoreType::Armv7m,
            probe_rs::CoreType::Armv7em => WireCoreType::Armv7em,
            probe_rs::CoreType::Armv8a => WireCoreType::Armv8a,
            probe_rs::CoreType::Armv8m => WireCoreType::Armv8m,
            probe_rs::CoreType::Riscv => WireCoreType::Riscv,
            probe_rs::CoreType::Riscv64 => WireCoreType::Riscv64,
            probe_rs::CoreType::Xtensa => WireCoreType::Xtensa,
        }
    }
}

impl From<WireCoreType> for probe_rs::CoreType {
    fn from(value: WireCoreType) -> Self {
        match value {
            WireCoreType::Armv6m => probe_rs::CoreType::Armv6m,
            WireCoreType::Armv7a => probe_rs::CoreType::Armv7a,
            WireCoreType::Armv7r => probe_rs::CoreType::Armv7r,
            WireCoreType::Armv7m => probe_rs::CoreType::Armv7m,
            WireCoreType::Armv7em => probe_rs::CoreType::Armv7em,
            WireCoreType::Armv8a => probe_rs::CoreType::Armv8a,
            WireCoreType::Armv8m => probe_rs::CoreType::Armv8m,
            WireCoreType::Riscv => probe_rs::CoreType::Riscv,
            WireCoreType::Riscv64 => probe_rs::CoreType::Riscv64,
            WireCoreType::Xtensa => probe_rs::CoreType::Xtensa,
        }
    }
}
