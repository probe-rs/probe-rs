//! Image loading and parsing for different file formats.

use ihex::Record;
use serde::{Deserialize, Serialize};

use std::io::{Read, Seek, SeekFrom};

use super::{extract_from_elf, BinOptions, FileDownloadError};
use crate::flashing::{FlashLoader, FormatKind};

/// Helper trait for object safety.
pub trait ImageReader: Read + Seek {}
impl<T> ImageReader for T where T: Read + Seek {}

fn load_file(reader: &mut dyn ImageReader) -> Result<Vec<u8>, FileDownloadError> {
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn load_string(reader: &mut dyn ImageReader) -> Result<String, FileDownloadError> {
    let mut buffer = String::new();
    reader.read_to_string(&mut buffer)?;
    Ok(buffer)
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum Format {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin(BinOptions),
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    #[default]
    Elf,
    /// Marks a file in the [UF2](https://github.com/microsoft/uf2) format.
    Uf2,
}

impl Format {
    /// Get the kind of the format.
    pub fn kind(&self) -> FormatKind {
        match self {
            Format::Bin(_) => FormatKind::Bin,
            Format::Hex => FormatKind::Hex,
            Format::Elf => FormatKind::Elf,
            Format::Uf2 => FormatKind::Uf2,
        }
    }

    /// Load the image from the given file into the flash loader.
    pub fn load(
        &self,
        flash_loader: &mut FlashLoader,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        match self {
            Format::Bin(options) => load_binary(options, flash_loader, file),
            Format::Elf => load_elf(flash_loader, file),
            Format::Hex => load_hex(flash_loader, file),
            Format::Uf2 => load_uf2(flash_loader, file),
        }
    }
}

impl From<FormatKind> for Format {
    fn from(kind: FormatKind) -> Self {
        match kind {
            FormatKind::Bin => Format::Bin(BinOptions::default()),
            FormatKind::Hex => Format::Hex,
            FormatKind::Elf => Format::Elf,
            FormatKind::Uf2 => Format::Uf2,
        }
    }
}

fn load_binary(
    options: &BinOptions,
    flash_loader: &mut FlashLoader,
    file: &mut dyn ImageReader,
) -> Result<(), FileDownloadError> {
    // Skip the specified bytes.
    file.seek(SeekFrom::Start(u64::from(options.skip)))?;

    let buf = load_file(file)?;

    flash_loader.add_data(
        // If no base address is specified use the start of the boot memory.
        // TODO: Implement this as soon as we know targets.
        options.base_address.unwrap_or_default(),
        &buf,
    )?;

    Ok(())
}

fn load_elf(
    flash_loader: &mut FlashLoader,
    file: &mut dyn ImageReader,
) -> Result<(), FileDownloadError> {
    let elf_buffer = load_file(file)?;

    let extracted_data = extract_from_elf(&elf_buffer)?;

    if extracted_data.is_empty() {
        tracing::warn!("No loadable segments were found in the ELF file.");
        return Err(FileDownloadError::NoLoadableSegments);
    }

    tracing::info!("Found {} loadable sections", extracted_data.len());

    for section in &extracted_data {
        let source = match section.section_names.len() {
            0 => "Unknown",
            1 => section.section_names[0].as_str(),
            _ => "Multiple sections",
        };

        tracing::info!(
            "    {} at {:#010X} ({} byte{})",
            source,
            section.address,
            section.data.len(),
            if section.data.len() == 1 { "" } else { "s" }
        );
    }

    for data in extracted_data {
        flash_loader.add_data(data.address.into(), data.data)?;
    }

    Ok(())
}

fn load_hex(
    flash_loader: &mut FlashLoader,
    file: &mut dyn ImageReader,
) -> Result<(), FileDownloadError> {
    let mut base_address = 0;

    let data = load_string(file)?;

    for record in ihex::Reader::new(&data) {
        match record? {
            Record::Data { offset, value } => {
                let offset = base_address + offset as u64;
                flash_loader.add_data(offset, &value)?;
            }
            Record::ExtendedSegmentAddress(address) => {
                base_address = (address as u64) * 16;
            }
            Record::ExtendedLinearAddress(address) => {
                base_address = (address as u64) << 16;
            }

            Record::EndOfFile
            | Record::StartSegmentAddress { .. }
            | Record::StartLinearAddress(_) => {}
        }
    }
    Ok(())
}

fn load_uf2(
    flash_loader: &mut FlashLoader,
    file: &mut dyn ImageReader,
) -> Result<(), FileDownloadError> {
    let uf2_buffer = load_file(file)?;

    let (converted, family_to_target) = uf2_decode::convert_from_uf2(&uf2_buffer).unwrap();
    let target_addresses = family_to_target.values();

    let Some(target_address) = target_addresses.min() else {
        tracing::warn!("No loadable segments were found in the UF2 file.");
        return Err(FileDownloadError::NoLoadableSegments);
    };

    let num_sections = family_to_target.len();
    tracing::info!("Found {num_sections} loadable sections");
    if num_sections > 1 {
        tracing::warn!("More than 1 section found in UF2 file.  Using first section.");
    }
    flash_loader.add_data(*target_address, &converted)?;

    Ok(())
}
