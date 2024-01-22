#![warn(missing_docs)]

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

mod chip;
mod chip_family;
mod flash_algorithm;
mod flash_properties;
mod memory;
pub(crate) mod serialize;

pub use chip::{
    ArmCoreAccessOptions, BinaryFormat, Chip, Core, CoreAccessOptions, Jtag,
    RiscvCoreAccessOptions, ScanChainElement, XtensaCoreAccessOptions,
};
pub use chip_family::{
    Architecture, ChipFamily, CoreType, InstructionSet, TargetDescriptionSource,
};
pub use flash_algorithm::{RawFlashAlgorithm, TransferEncoding};
pub use flash_properties::FlashProperties;
pub use memory::{
    GenericRegion, MemoryRange, MemoryRegion, NvmRegion, PageInfo, RamRegion, SectorDescription,
    SectorInfo,
};
