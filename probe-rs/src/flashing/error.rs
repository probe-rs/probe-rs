#![allow(missing_docs)]

use thiserror::Error;

use crate::config::{FlashRegion, TargetDescriptionSource};
use crate::error;

/// Describes any error that happened during the or in preparation for the flashing procedure.
#[derive(Error, Debug)]
pub enum FlashError {
    // End-user errors

    // 1 List regions configured in target description
    // 2 Show hints on how to solve this:
    //   - No linker script -> see cortex-m / embedded book / other useful ressource (maybe put in probe-rs-cli-util?)
    //   - Wrong linker script
    //   - Wrong target definition
    // 3 Show origin of memory range (elf section / program header)
    // 4 Find best match in target description
    #[error(
        "No flash memory contains the entire requested memory range {start:#08X}..{end:#08X}."
    )]
    NoSuitableFlash {
        start: u32,
        end: u32,
        description_source: TargetDescriptionSource,
    },

    // 1 Add information about flash (name, address)
    // 2 Add source of target definition (built-in, yaml)
    #[error("Trying to write flash, but no suitable flash loader algorithm is linked to the given target information.")]
    NoFlashLoaderAlgorithmAttached,

    // 1 Add name of chip
    // 2 Add source of target definition
    #[error("No RAM defined for chip.")]
    NoRamDefined,

    // Add address of sector to error
    #[error("The execution of '{name}' failed with code {errorcode}. Perhaps your chip has write protected sectors that need to be cleared? Perhaps you need the --nmagic linker arg https://github.com/rust-embedded/cortex-m-quickstart/pull/95")]
    EraseFailed { name: &'static str, errorcode: u32 },
    #[error(
        "The page write of the page at address {page_address:#08X} failed with error code {error_code}."
    )]
    PageWrite { page_address: u32, error_code: u32 },

    #[error("The '{0}' routine is not supported with the given flash algorithm.")]
    RoutineNotSupported(&'static str), // TODO: Rename to erase_all not supported

    // Mostly internal error, should probably be a bug report
    #[error("The execution of '{name}' failed with code {errorcode}")]
    RoutineCallFailed { name: &'static str, errorcode: u32 }, // probably an issue with the flash algorithm / target

    // Libary API error?
    #[error("{address} is not contained in {region:?}")]
    AddressNotInRegion { address: u32, region: FlashRegion },

    // Group Memory and Core (connection / communication problem?)
    #[error("Something during memory interaction went wrong")]
    Memory(#[source] error::Error),
    #[error("Something during the interaction with the core went wrong")]
    Core(#[source] error::Error),

    // Flash algorithm in YAML is broken
    #[error("Flash algorithm length is not 32 bit aligned.")]
    InvalidFlashAlgorithmLength {
        name: String,
        algorithm_source: Option<TargetDescriptionSource>,
    },

    // Remove this error?
    #[error(
        "The RAM contents did not match the expected contents after loading the flash algorithm."
    )]
    FlashAlgorithmNotLoaded,
}
