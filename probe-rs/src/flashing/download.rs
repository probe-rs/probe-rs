use probe_rs_target::InstructionSet;
#[cfg(feature = "builtin-formats")]
use serde::{Deserialize, Serialize};

use std::{fs::File, path::Path};

use super::*;
use crate::session::Session;

/// Extended options for flashing a binary file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
#[cfg(feature = "builtin-formats")]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file.
    pub skip: u32,
}

/// Extended options for flashing an ELF file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
#[cfg(feature = "builtin-formats")]
pub struct ElfOptions {
    /// Sections to skip flashing
    pub skip_sections: Vec<String>,
}

/// A finite list of all the errors that can occur when flashing a given file.
///
/// This includes corrupt file issues,
/// OS permission issues as well as chip connectivity and memory boundary issues.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum FileDownloadError {
    /// An error with the flashing procedure has occurred.
    #[ignore_extra_doc_attributes]
    ///
    /// This is mostly an error in the communication with the target inflicted by a bad hardware connection or a probe-rs bug.
    Flash(#[from] FlashError),

    /// Failed to read or decode the IHEX file.
    #[cfg(feature = "builtin-formats")]
    IhexRead(#[from] ihex::ReaderError),

    /// An IO error has occurred while reading the firmware file.
    IO(#[from] std::io::Error),

    /// Error while reading the object file: {0}.
    Object(&'static str),

    /// Failed to read or decode the ELF file.
    #[cfg(feature = "builtin-formats")]
    Elf(#[from] object::read::Error),

    /// An error specific to the {format} image format has occurred.
    ImageFormatSpecific {
        /// The image format.
        format: String,

        /// The specific error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// No loadable segments were found in the ELF file.
    #[ignore_extra_doc_attributes]
    ///
    /// This is most likely because of a bad linker script.
    NoLoadableSegments,

    /// The image ({image:?}) is not compatible with the target ({print_instr_sets(target)}).
    IncompatibleImage {
        /// The target's instruction set.
        target: Vec<InstructionSet>,
        /// The image's instruction set.
        image: InstructionSet,
    },

    /// The target chip {target} is not compatible with the image. The image is compatible with: {image_chips.join(", ")}
    IncompatibleImageChip {
        /// The target chip.
        target: String,
        /// The chips compatible with the image.
        image_chips: Vec<String>,
    },

    /// An error occurred during download.
    Other(#[source] crate::Error),
}

fn print_instr_sets(instr_sets: &[InstructionSet]) -> String {
    instr_sets
        .iter()
        .map(|instr_set| format!("{instr_set:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Options for downloading a file onto a target chip.
///
/// This struct should be created using the [`DownloadOptions::default()`] function, and can be configured by setting
/// the fields directly:
///
/// ```
/// use probe_rs::flashing::DownloadOptions;
///
/// let mut options = DownloadOptions::default();
///
/// options.verify = true;
/// ```
#[derive(Default)]
#[non_exhaustive]
pub struct DownloadOptions<'p> {
    /// An optional progress reporter which is used if this argument is set to `Some(...)`.
    pub progress: FlashProgress<'p>,
    /// If `keep_unwritten_bytes` is `true`, erased portions of the flash that are not overwritten by the ELF data
    /// are restored afterwards, such that the old contents are untouched.
    ///
    /// This is necessary because the flash can only be erased in sectors. If only parts of the erased sector are written thereafter,
    /// instead of the full sector, the excessively erased bytes wont match the contents before the erase which might not be intuitive
    /// to the user or even worse, result in unexpected behavior if those contents contain important data.
    pub keep_unwritten_bytes: bool,
    /// Perform a dry run. This prepares everything for flashing, but does not write anything to flash.
    pub dry_run: bool,
    /// If this flag is set to true, probe-rs will try to use the chips built in method to do a full chip erase if one is available.
    /// This is often faster than erasing a lot of single sectors.
    /// So if you do not need the old contents of the flash, this is a good option.
    pub do_chip_erase: bool,
    /// If the chip was pre-erased with external erasers, this flag can set to true to skip erasing
    /// It may be useful for mass production.
    pub skip_erase: bool,
    /// Before flashing, read back the flash contents to skip up-to-date regions.
    pub preverify: bool,
    /// After flashing, read back all the flashed data to verify it has been written correctly.
    pub verify: bool,
    /// Disable double buffering when loading flash.
    pub disable_double_buffering: bool,
    /// If there are multiple valid flash algorithms for a memory region, this list allows
    /// overriding the default selection.
    pub preferred_algos: Vec<String>,
}

impl DownloadOptions<'_> {
    /// DownloadOptions with default values.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Builds a new flash loader for the given target and path. This
/// will check the path for validity and check what pages have to be
/// flashed etc.
pub fn build_loader(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: impl ImageLoader,
    image_instruction_set: Option<InstructionSet>,
) -> Result<FlashLoader, FileDownloadError> {
    // Create the flash loader
    let mut loader = session.target().flash_loader();

    // Add data from the BIN.
    let mut file = File::open(path).map_err(FileDownloadError::IO)?;

    loader.load_image(session, &mut file, format, image_instruction_set)?;

    Ok(loader)
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory boundaries are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for more options, have a look at [download_file_with_options].
pub fn download_file(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: impl ImageLoader,
) -> Result<(), FileDownloadError> {
    download_file_with_options(session, path, format, DownloadOptions::default())
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory boundaries are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for a simple version without many options, have a look at [download_file].
pub fn download_file_with_options(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: impl ImageLoader,
    options: DownloadOptions,
) -> Result<(), FileDownloadError> {
    let loader = build_loader(session, path, format, None)?;

    loader
        .commit(session, options)
        .map_err(FileDownloadError::Flash)
}
