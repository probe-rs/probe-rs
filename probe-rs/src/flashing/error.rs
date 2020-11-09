use thiserror::Error;

use crate::config::FlashRegion;
use crate::error;

/// Describes any error that happened during the or in preparation for the flashing procedure.
#[derive(Error, Debug)]
pub enum FlashError {
    #[error("The execution of '{name}' failed with code {errorcode}. Perhaps your chip has write protected sectors that need to be cleared? Perhaps you need the --nmagic linker arg https://github.com/rust-embedded/cortex-m-quickstart/pull/95")]
    EraseFailed { name: &'static str, errorcode: u32 },
    #[error("The execution of '{name}' failed with code {errorcode}")]
    RoutineCallFailed { name: &'static str, errorcode: u32 },
    #[error("The '{0}' routine is not supported with the given flash algorithm.")]
    RoutineNotSupported(&'static str),
    #[error("Buffer {n}/{max} does not exist")]
    InvalidBufferNumber { n: usize, max: usize },
    #[error("Something during memory interaction went wrong")]
    Memory(#[source] error::Error),
    #[error("Something during the interaction with the core went wrong")]
    Core(#[source] error::Error),
    #[error("{address} is not contained in {region:?}")]
    AddressNotInRegion { address: u32, region: FlashRegion },
    #[error("Flash algorithm length is not 32 bit aligned.")]
    InvalidFlashAlgorithmLength,
    #[error(
        "The RAM contents did not match the expected contents after loading the flash algorithm."
    )]
    FlashAlgorithmNotLoaded,
    #[error(
        "The page write of the page at address {page_address:#08X} failed with error code {error_code}."
    )]
    PageWrite { page_address: u32, error_code: u32 },
    #[error("Overlap in data, address {0:#010x} was already written earlier.")]
    DataOverlap(u32),
    #[error("Address {0:#010x} is not a valid address in the flash area.")]
    InvalidFlashAddress(u32),
    #[error(
        "No flash memory contains the entire requested memory range {start:#08X}..{end:#08X}."
    )]
    NoSuitableFlash { start: u32, end: u32 },
    #[error("Trying to write flash, but no suitable flash loader algorithm is linked to the given target information.")]
    NoFlashLoaderAlgorithmAttached,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
