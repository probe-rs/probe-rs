//! Target specific configuration
//!
//! For debugging and flashing different chips, called *target* in probe-rs, some
//! target specific configuration is required. This includes the architecture of
//! the chip, e.g. RISC-V or ARM, and information about the memory map of the target,
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
//! To add a target at runtime, the [add_target_from_yaml] function can
//! be used to read targets from a YAML file.
//!

mod chip_info;
pub(crate) mod registry;
mod target;

pub use probe_rs_target::{
    Chip, ChipFamily, Core, CoreType, FlashProperties, GenericRegion, InstructionSet, MemoryAccess,
    MemoryRange, MemoryRegion, NvmRegion, PageInfo, RamRegion, RawFlashAlgorithm, ScanChainElement,
    SectorDescription, SectorInfo, TargetDescriptionSource,
};

pub use registry::{
    add_target_from_yaml, families, get_target_and_family_by_name, get_target_by_name,
    get_targets_by_family_name, search_chips, RegistryError,
};
pub use target::{CoreDebugSequence, DebugSequence, Target, TargetSelector};

// Crate-internal API
pub(crate) use chip_info::ChipInfo;
pub(crate) use registry::get_target_by_chip_info;
pub(crate) use target::CoreExt;
