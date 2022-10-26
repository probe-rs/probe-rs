#![warn(missing_docs)]

//! Target specific configuration
//!
//! For debugging and flashing different chips, called *target* in probe-rs, some
//! target specific configuration is required. This includes the architecture of
//! the chip, e.g. RISCV or ARM, and information about the memory map of the target,
//! which can be used together with a flash algorithm to program the flash memory
//! of a target.
//!
//! ## Built-in targets
//!
//! The built-in targets are added at build-time, from the `build.rs` script.
//! They are generated from the target files in the `targets/` subfolder of this
//! crate.
//!
//! The built-in targets can be disabled by not including the `builtin-targets` feature.
//!
//! ## Adding targets at runtime
//!
//! To add a target at runtime, the [add_target_from_yaml] file can
//! be used to read targets from a YAML file.
//!

mod chip_info;
mod registry;
mod target;

pub use probe_rs_target::{
    Chip, ChipFamily, Core, CoreType, FlashProperties, GenericRegion, InstructionSet, MemoryRange,
    MemoryRegion, NvmRegion, PageInfo, RamRegion, RawFlashAlgorithm, SectorDescription, SectorInfo,
    TargetDescriptionSource,
};

pub use registry::{
    add_target_from_yaml, families, get_target_by_name, search_chips, RegistryError,
};
pub use target::{DebugSequence, Target, TargetParseError, TargetSelector};

// Crate-internal API
pub(crate) use chip_info::ChipInfo;
pub(crate) use registry::get_target_by_chip_info;
