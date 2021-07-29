#![allow(missing_docs)]

use crate::config::{NvmRegion, RamRegion, TargetDescriptionSource};
use crate::error;
use std::ops::Range;

/// Describes any error that happened during the or in preparation for the flashing procedure.
#[derive(thiserror::Error, Debug)]
pub enum FlashError {
    #[error(
        "No flash memory contains the entire requested memory range {start:#010x}..{end:#10x}."
    )]
    NoSuitableNvm {
        start: u32,
        end: u32,
        description_source: TargetDescriptionSource,
    },
    #[error("Failed to erase the whole chip.")]
    ChipEraseFailed {
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    #[error("Failed to erase flash sector at address {sector_address:#010x}.")]
    EraseFailed {
        sector_address: u32,
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    #[error("The page write of the page at address {page_address:#010x} failed.")]
    PageWrite {
        page_address: u32,
        source: Box<dyn std::error::Error + 'static + Send + Sync>,
    },
    #[error("The initialization of the flash algorithm failed.")]
    Init(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    #[error("The uninitialization of the flash algorithm failed.")]
    Uninit(#[source] Box<dyn std::error::Error + 'static + Send + Sync>),
    #[error("The chip erase routine is not supported with the given flash algorithm.")]
    ChipEraseNotSupported,
    #[error("The execution of '{name}' failed with code {error_code}. This might indicate a problem with the flash algorithm.")]
    RoutineCallFailed { name: &'static str, error_code: u32 },
    #[error("{address:#010x} is not contained in {region:?}")]
    AddressNotInRegion { address: u32, region: NvmRegion },
    #[error("Something during the interaction with the core went wrong")]
    Core(#[source] error::Error),
    #[error(
        "The RAM contents did not match the expected contents after loading the flash algorithm."
    )]
    FlashAlgorithmNotLoaded,

    // TODO: Warn at YAML parsing stage.
    // TODO: 1 Add information about flash (name, address)
    // TODO: 2 Add source of target definition (built-in, yaml)
    #[error("Trying to write flash, but no suitable (default) flash loader algorithm is linked to the given target: {name} .")]
    NoFlashLoaderAlgorithmAttached { name: String },

    #[error("Trying to write flash, but found more than one suitable flash loader algorithim marked as default for {region:?}.")]
    MultipleDefaultFlashLoaderAlgorithms { region: NvmRegion },
    #[error("Trying to write flash, but found more than one suitable flash algorithims but none marked as default for {region:?}.")]
    MultipleFlashLoaderAlgorithmsNoDefault { region: NvmRegion },

    #[error("Verify failed.")]
    Verify,

    // TODO: 1 Add source of target definition
    #[error("No RAM defined for chip: {chip}.")]
    NoRamDefined { chip: String },

    // Flash algorithm in YAML is broken
    #[error("Flash algorithm length is not 32 bit aligned.")]
    InvalidFlashAlgorithmLength {
        name: String,
        algorithm_source: Option<TargetDescriptionSource>,
    },
    #[error("Adding data for addresses {added_addresses:08X?} overlaps previously added data for addresses {existing_addresses:08X?}.")]
    DataOverlaps {
        added_addresses: Range<u32>,
        existing_addresses: Range<u32>,
    },
    #[error("No core can access the NVM region {0:?}.")]
    NoNvmCoreAccess(NvmRegion),
    #[error("No core can access the ram region {0:?}.")]
    NoRamCoreAccess(RamRegion),
}
