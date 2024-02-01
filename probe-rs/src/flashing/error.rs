use crate::config::{NvmRegion, RamRegion, TargetDescriptionSource};
use crate::error;
use std::ops::Range;

/// Describes any error that happened during the or in preparation for the flashing procedure.
#[derive(thiserror::Error, displaydoc::Display, Debug)]
pub enum FlashError {
    /// "No flash memory contains the entire requested memory range: {start:#010x}..{end:#010x}.
    NoSuitableNvm {
        /// The start of the requested memory range.
        start: u64,
        /// The end of the requested memory range.
        end: u64,
        /// The source of this target description (was it a built in target or one loaded externally and from what file path?).
        description_source: TargetDescriptionSource,
    },
    /// Failed to erase the whole chip.
    ChipEraseFailed {
        /// The source error of this error.
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// Failed to erase flash sector at address {sector_address:#010x}.
    EraseFailed {
        /// The address of the sector that should have been erased.
        sector_address: u64,
        /// The source error of this error.
        #[source]
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// The page write of the page at address {page_address:#010x} failed.
    PageWrite {
        /// The address of the page that should have been written.
        page_address: u64,
        /// The source error of this error.
        #[source]
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// The initialization of the flash algorithm failed.
    Init(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    /// The uninitialization of the flash algorithm failed.
    Uninit(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    /// The chip erase routine is not supported with the given flash algorithm.
    ChipEraseNotSupported,
    /// The execution of '{name}' failed with code {error_code}. This might indicate a problem with the flash algorithm.
    RoutineCallFailed {
        /// The name of the routine that was called.
        name: &'static str,
        /// The error code the called routine returned.
        error_code: u32,
    },
    /// The core entered an unexpected status: {status:?}.
    UnexpectedCoreStatus {
        /// The status that the core entered.
        status: crate::CoreStatus,
    },
    /// {address:#010x} is not contained in {region:?}.
    AddressNotInRegion {
        /// The address which was not contained in `region`.
        address: u32,
        /// The region which did not contain `address`.
        region: NvmRegion,
    },
    /// Something during the interaction with the core went wrong.
    Core(#[from] error::Error),
    /// The RAM contents did not match the expected contents after loading the flash algorithm.
    FlashAlgorithmNotLoaded,
    /**
     * Failed to load the flash algorithm into RAM at given address. This can happen if there is not enough space.
     *
     * Check the algorithm code and settings before you try again.
     */
    InvalidFlashAlgorithmLoadAddress {
        /// The address where the algorithm was supposed to be loaded to.
        address: u64,
    },
    /// Invalid page size {size:08X?}. Must be a multiple of 4 bytes.
    InvalidPageSize {
        /// The size of the page in bytes.
        size: u32,
    },
    // TODO: Warn at YAML parsing stage.
    // TODO: 1 Add information about flash (name, address)
    // TODO: 2 Add source of target definition (built-in, yaml)
    /// Trying to write flash, but no suitable (default) flash loader algorithm is linked to the given target: {name}.
    NoFlashLoaderAlgorithmAttached {
        /// The name of the chip.
        name: String,
    },
    /// Trying to write flash, but found more than one suitable flash loader algorithm marked as default for {region:?}.
    MultipleDefaultFlashLoaderAlgorithms {
        /// The region which matched more than one flash algorithm.
        region: NvmRegion,
    },
    /// Trying to write flash, but found more than one suitable flash algorithms but none marked as default for {region:?}.
    MultipleFlashLoaderAlgorithmsNoDefault {
        /// The region which matched more than one flash algorithm.
        region: NvmRegion,
    },
    /// Flash content verification failed.
    Verify,
    // TODO: 1 Add source of target definition
    // TOOD: 2 Do this at target load time.
    /// No RAM defined for target: {name}.
    NoRamDefined {
        /// The name of the chip.
        name: String,
    },
    /**
     * Flash algorithm {name} does not have a length that is 4 byte aligned.
     *
     * This means that the flash algorithm that was loaded is broken.
     */
    InvalidFlashAlgorithmLength {
        /// The name of the flash algorithm.
        name: String,
        /// The source of the flash algorithm (was it a built in target or one loaded externally and from what file path?).
        algorithm_source: Option<TargetDescriptionSource>,
    },
    /**
     * Adding data for addresses {added_addresses:08X?} overlaps previously added data for addresses {existing_addresses:08X?}.
     *
     * This means the loaded binary is broken. Please check your data and try again.
     */
    DataOverlaps {
        /// The address range that was tried to be added.
        added_addresses: Range<u64>,
        /// The address range that was already present.
        existing_addresses: Range<u64>,
    },
    /// No core can access the NVM region {0:?}.
    NoNvmCoreAccess(NvmRegion),
    /// No core can access the ram region {0:?}.
    NoRamCoreAccess(RamRegion),
    /// The register value {0:08X?} is out of the supported range.
    RegisterValueNotSupported(u64),
}
