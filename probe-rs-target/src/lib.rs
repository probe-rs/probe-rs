//! Target description schema
//!
//! For debugging and flashing different chips, called *target* in probe-rs, some
//! target specific configuration is required. This includes the architecture of
//! the chip, e.g. RISC-V or ARM, and information about the memory map of the target,
//! which can be used together with a flash algorithm to program the flash memory
//! of a target.
//!
//! This crate contains the schema structs for the YAML target description files.
//!
#![warn(missing_docs)]

mod chip;
pub mod chip_detection;
mod chip_family;
mod flash_algorithm;
mod flash_properties;
mod memory;
pub(crate) mod serialize;

pub use chip::{
    ArmCoreAccessOptions, Chip, Core, CoreAccessOptions, Jtag, RiscvCoreAccessOptions,
    RiscvJtagTunnel, ScanChainElement, XtensaCoreAccessOptions,
};
pub use chip_family::{
    Architecture, ChipFamily, CoreType, InstructionSet, TargetDescriptionSource,
};
pub use flash_algorithm::{RawFlashAlgorithm, TransferEncoding};
pub use flash_properties::FlashProperties;
pub use memory::{
    GenericRegion, MemoryAccess, MemoryRange, MemoryRegion, NvmRegion, PageInfo, RamRegion,
    RegionMergeIterator, SectorDescription, SectorInfo,
};
