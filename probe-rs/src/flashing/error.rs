use crate::config::{NvmRegion, RamRegion, TargetDescriptionSource};
use crate::error;
use std::ops::Range;

/// Describes any error that happened during the or in preparation for the flashing procedure.
#[derive(thiserror::Error, Debug)]
pub enum FlashError {
    /// No flash memory contains the entire requested memory range.
    #[error(
        "No flash memory contains the entire requested memory range {start:#010x}..{end:#010x}."
    )]
    NoSuitableNvm {
        /// The start of the requested memory range.
        start: u64,
        /// The end of the requested memory range.
        end: u64,
        /// The source of this target description (was it a built in target or one loaded externally and from what file path?).
        description_source: TargetDescriptionSource,
    },
    /// Erasing the full chip flash failed.
    #[error("Failed to erase the whole chip.")]
    ChipEraseFailed {
        /// The source error of this error.
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// Erasing the given flash sector failed.
    #[error("Failed to erase flash sector at address {sector_address:#010x}.")]
    EraseFailed {
        /// The address of the sector that should have been erased.
        sector_address: u64,
        /// The source error of this error.
        #[source]
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// Writing the given page failed.
    #[error("The page write of the page at address {page_address:#010x} failed.")]
    PageWrite {
        /// The address of the page that should have been written.
        page_address: u64,
        /// The source error of this error.
        #[source]
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    /// Initializing the flash algorithm failed.
    #[error("The initialization of the flash algorithm failed.")]
    Init(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    /// Uninizializing the flash algorithm failed.
    #[error("The uninitialization of the flash algorithm failed.")]
    Uninit(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    /// This target does not support full chip flash erases.
    #[error("The chip erase routine is not supported with the given flash algorithm.")]
    ChipEraseNotSupported,
    /// Calling the given routine returned the given error code.
    #[error("The execution of '{name}' failed with code {error_code}. This might indicate a problem with the flash algorithm.")]
    RoutineCallFailed {
        /// The name of the routine that was called.
        name: &'static str,
        /// The error code the called routine returned.
        error_code: u32,
    },
    /// The core entered an unexpected status while executing a flashing operation.
    #[error("The core entered an unexpected status: {status:?}.")]
    UnexpectedCoreStatus {
        /// The status that the core entered.
        status: crate::CoreStatus,
    },
    /// The given address was not contained in the given NVM region.
    #[error("{address:#010x} is not contained in {region:?}")]
    AddressNotInRegion {
        /// The address which was not contained in `region`.
        address: u32,
        /// The region which did not contain `address`.
        region: NvmRegion,
    },
    /// An error occurred during the interaction with the core.
    #[error("Something during the interaction with the core went wrong")]
    Core(#[from] error::Error),
    /// The RAM contents did not match the flash algorithm.
    #[error(
        "The RAM contents did not match the expected contents after loading the flash algorithm."
    )]
    FlashAlgorithmNotLoaded,
    /// Failed to load the flash algorithm into RAM at given address. This can happen if there is not enough space.
    ///
    /// Check the algorithm code and settings before you try again.
    #[error(
        "Failed to load flash algorithm into RAM at address {address:08X?}. Is there space for the algorithm header?"
    )]
    InvalidFlashAlgorithmLoadAddress {
        /// The address where the algorithm was supposed to be loaded to.
        address: u64,
    },
    /// The given page size is not valid. Only page sizes multiples of 4 bytes are allowed.
    #[error("Invalid page size {size:08X?}. Must be a multiple of 4 bytes.")]
    InvalidPageSize {
        /// The size of the page in bytes.
        size: u32,
    },
    // TODO: Warn at YAML parsing stage.
    // TODO: 1 Add information about flash (name, address)
    // TODO: 2 Add source of target definition (built-in, yaml)
    /// No flash algorithm was linked to this target.
    #[error("Trying to write flash, but no suitable (default) flash loader algorithm is linked to the given target: {name} .")]
    NoFlashLoaderAlgorithmAttached {
        /// The name of the chip.
        name: String,
    },
    /// More than one matching flash algorithm was found for the given memory range and all of them is marked as default.
    #[error("Trying to write flash, but found more than one suitable flash loader algorithim marked as default for {region:?}.")]
    MultipleDefaultFlashLoaderAlgorithms {
        /// The region which matched more than one flash algorithm.
        region: NvmRegion,
    },
    /// More than one matching flash algorithm was found for the given memory range and none of them is marked as default.
    #[error("Trying to write flash, but found more than one suitable flash algorithims but none marked as default for {region:?}.")]
    MultipleFlashLoaderAlgorithmsNoDefault {
        /// The region which matched more than one flash algorithm.
        region: NvmRegion,
    },
    /// Flash content verification failed.
    #[error("Flash content verification failed.")]
    Verify,
    // TODO: 1 Add source of target definition
    // TOOD: 2 Do this at target load time.
    /// The given chip has no RAM defined.
    #[error("No RAM defined for target: {name}.")]
    NoRamDefined {
        /// The name of the chip.
        name: String,
    },
    /// The given flash algorithm did not have a leng multiple of 4 bytes.
    ///
    /// This means that the flash algorithm that was loaded is broken.
    #[error("Flash algorithm {name} does not have a length that is 4 byte aligned.")]
    InvalidFlashAlgorithmLength {
        /// The name of the flash algorithm.
        name: String,
        /// The source of the flash algorithm (was it a built in target or one loaded externally and from what file path?).
        algorithm_source: Option<TargetDescriptionSource>,
    },
    /// Two blocks of data overlap each other which means the loaded binary is broken.
    ///
    /// Please check your data and try again.
    #[error("Adding data for addresses {added_addresses:08X?} overlaps previously added data for addresses {existing_addresses:08X?}.")]
    DataOverlaps {
        /// The address range that was tried to be added.
        added_addresses: Range<u64>,
        /// The address range that was already present.
        existing_addresses: Range<u64>,
    },
    /// No core can access this NVM region.
    #[error("No core can access the NVM region {0:?}.")]
    NoNvmCoreAccess(NvmRegion),
    /// No core can access this RAM region.
    #[error("No core can access the ram region {0:?}.")]
    NoRamCoreAccess(RamRegion),
    /// The register value supplied for this flash algorithm is out of the supported range.
    #[error("The register value {0:08X?} is out of the supported range.")]
    RegisterValueNotSupported(u64),
}
