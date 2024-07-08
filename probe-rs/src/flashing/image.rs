//! Supported image formats
use crate::{
    flashing::{extract_from_elf, FileDownloadError, FlashLoader},
    session::Session,
};
use ihex::Record;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use std::{
    io::{Read, Seek, SeekFrom},
    str::FromStr,
};

/// A finite list of all the available binary formats probe-rs understands.
#[serde_as]
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum FormatKind {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin,
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    #[default]
    Elf,
    /// Marks a file in the [UF2](https://github.com/microsoft/uf2) format.
    Uf2,
    /// A vendor-specific image format.
    VendorSpecific(#[serde_as(as = "DisplayFromStr")] ImageFormat),
}

impl FormatKind {
    /// Creates a new Format from an optional string.
    ///
    /// If the string is `None`, the default format is returned.
    pub fn from_optional(s: Option<&str>) -> Result<Self, String> {
        match s {
            Some(format) => Self::from_str(format),
            None => Ok(Self::default()),
        }
    }
}

impl FromStr for FormatKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_lowercase()[..] {
            "bin" | "binary" => Ok(Self::Bin),
            "hex" | "ihex" | "intelhex" => Ok(Self::Hex),
            "elf" => Ok(Self::Elf),
            "uf2" => Ok(Self::Uf2),
            other => Ok(Self::VendorSpecific(ImageFormat::from_str(other)?)),
        }
    }
}

/// Operations related to a vendor-specific image format.
pub trait ImageFormatDefinition: FormatClone + Send + Sync {
    /// Returns the name of the image format.
    fn name(&self) -> &'static str;

    /// Returns a default image loader for this format.
    fn default(&self) -> Box<dyn ImageLoader>;
}

#[doc(hidden)]
pub trait FormatClone {
    fn clone_box(&self) -> Box<dyn ImageFormatDefinition>;
}

impl<T> FormatClone for T
where
    T: 'static + ImageFormatDefinition + Clone,
{
    fn clone_box(&self) -> Box<dyn ImageFormatDefinition> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn ImageFormatDefinition> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// A handler for a vendor-specific image format.
#[derive(Clone)]
pub struct ImageFormat(Box<dyn ImageFormatDefinition>);

impl<T> From<T> for ImageFormat
where
    T: ImageFormatDefinition + 'static,
{
    fn from(format: T) -> Self {
        Self(Box::new(format))
    }
}

impl ImageFormat {
    /// Returns the name of the image format.
    pub fn name(&self) -> &'static str {
        self.0.name()
    }

    /// Returns a default image loader for this format.
    pub fn default(&self) -> Box<dyn ImageLoader> {
        self.0.default()
    }
}

impl PartialEq for ImageFormat {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}
impl Eq for ImageFormat {}

impl std::fmt::Debug for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ImageFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::vendor::try_parse_image_format(s)
            .ok_or_else(|| format!("Unknown image format '{s}'"))
    }
}

/// Helper trait for object safety.
pub trait ImageReader: Read + Seek {}
impl<T> ImageReader for T where T: Read + Seek {}

/// Load and parse a firmware in a particular format, and add it to the flash loader.
///
/// Based on the image loader, probe-rs may apply certain transformations to the firmware.
pub trait ImageLoader {
    /// Loads the given image.
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError>;
}

/// An initialized firmware image loader.
pub struct Format(Box<dyn ImageLoader>);

impl Format {
    /// Loads the given image.
    pub fn load(
        &self,
        flash_loader: &mut FlashLoader,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        self.0.load(flash_loader, session, file)
    }
}

impl From<FormatKind> for Format {
    fn from(kind: FormatKind) -> Self {
        match kind {
            FormatKind::Bin => Self::from(BinLoader(BinOptions::default())),
            FormatKind::Hex => Self::from(HexLoader),
            FormatKind::Elf => Self::from(ElfLoader),
            FormatKind::Uf2 => Self::from(Uf2Loader),
            FormatKind::VendorSpecific(f) => Self(f.default()),
        }
    }
}

impl<I> From<I> for Format
where
    I: ImageLoader + 'static,
{
    fn from(loader: I) -> Self {
        Self(Box::new(loader))
    }
}

/// Extended options for flashing a binary file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file.
    pub skip: u32,
}

/// Reads the data from the binary file and adds it to the loader without splitting it into flash instructions yet.
pub struct BinLoader(pub BinOptions);

impl ImageLoader for BinLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        // Skip the specified bytes.
        file.seek(SeekFrom::Start(u64::from(self.0.skip)))?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        flash_loader.add_data(
            // If no base address is specified use the start of the boot memory.
            // TODO: Implement this as soon as we know targets.
            self.0.base_address.unwrap_or_default(),
            &buf,
        )?;

        Ok(())
    }
}

/// Prepares the data sections that have to be loaded into flash from an ELF file.
/// This will validate the ELF file and transform all its data into sections but no flash loader commands yet.
pub struct ElfLoader;

impl ImageLoader for ElfLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut elf_buffer = Vec::new();
        file.read_to_end(&mut elf_buffer)?;

        let extracted_data = extract_from_elf(&elf_buffer)?;

        if extracted_data.is_empty() {
            tracing::warn!("No loadable segments were found in the ELF file.");
            return Err(FileDownloadError::NoLoadableSegments);
        }

        tracing::info!("Found {} loadable sections:", extracted_data.len());

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
}

/// Reads the HEX data segments and adds them as loadable data blocks to the loader.
/// This does not create any flash loader instructions yet.
pub struct HexLoader;

impl ImageLoader for HexLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut base_address = 0;

        let mut data = String::new();
        file.read_to_string(&mut data)?;

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
}

/// Prepares the data sections that have to be loaded into flash from an UF2 file.
/// This will validate the UF2 file and transform all its data into sections but no flash loader commands yet.
pub struct Uf2Loader;

impl ImageLoader for Uf2Loader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut uf2_buffer = Vec::new();
        file.read_to_end(&mut uf2_buffer)?;

        let (converted, family_to_target) = uf2_decode::convert_from_uf2(&uf2_buffer).unwrap();
        let target_addresses = family_to_target.values();
        let num_sections = family_to_target.len();

        if let Some(target_address) = target_addresses.min() {
            tracing::info!("Found {} loadable sections:", num_sections);
            if num_sections > 1 {
                tracing::warn!("More than 1 section found in UF2 file.  Using first section.");
            }
            flash_loader.add_data(*target_address, &converted)?;

            Ok(())
        } else {
            tracing::warn!("No loadable segments were found in the UF2 file.");
            Err(FileDownloadError::NoLoadableSegments)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_format() {
        assert_eq!(FormatKind::from_str("hex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Hex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Ihex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("IHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("iHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("IntelHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("intelhex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("intelHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Intelhex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("bin"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Bin"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("binary"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Binary"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Elf"), Ok(FormatKind::Elf));
        assert_eq!(FormatKind::from_str("elf"), Ok(FormatKind::Elf));
        assert_eq!(
            FormatKind::from_str("idf"),
            Ok(FormatKind::VendorSpecific(
                ImageFormat::from_str("idf").unwrap()
            ))
        );
        assert_eq!(
            FormatKind::from_str("espidf"),
            Ok(FormatKind::VendorSpecific(
                ImageFormat::from_str("espidf").unwrap()
            ))
        );
        assert_eq!(
            FormatKind::from_str("esp-idf"),
            Ok(FormatKind::VendorSpecific(
                ImageFormat::from_str("esp-idf").unwrap()
            ))
        );
        assert_eq!(
            FormatKind::from_str("ESP-IDF"),
            Ok(FormatKind::VendorSpecific(
                ImageFormat::from_str("ESP-IDF").unwrap()
            ))
        );
        assert_eq!(
            FormatKind::from_str("elfbin"),
            Err("Format 'elfbin' is unknown.".to_string())
        );
        assert_eq!(
            FormatKind::from_str(""),
            Err("Format '' is unknown.".to_string())
        );
        assert_eq!(
            FormatKind::from_str("asdasdf"),
            Err("Format 'asdasdf' is unknown.".to_string())
        );
    }
}
