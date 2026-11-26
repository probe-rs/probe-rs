use std::io::Cursor;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use espflash::flasher::{FlashData, FlashFrequency, FlashMode, FlashSettings, FlashSize};
use espflash::image_format::idf::{IdfBootloaderFormat, check_idf_bootloader};
use probe_rs::config::{MemoryRegion, Target};
use probe_rs::flashing::{
    ElfLoader, ElfOptions, FileDownloadError, FlashError, FlashLoader, FlashProgress, Flasher,
    ImageContext, ImageFormat, ImageLoader, ImageReader, into_format_error,
};
use probe_rs::meta::{ElfMetadata, keys};
use serde::{Deserialize, Serialize};
use yaml_serde::Value;

pub(crate) struct IdfLoaderFactory;

impl ImageFormat for IdfLoaderFactory {
    fn formats(&self) -> &[&str] {
        &["idf", "esp-idf", "espidf"]
    }

    fn create_loader(&self, options: Option<Value>) -> Box<dyn ImageLoader> {
        let options = options
            .and_then(|value| yaml_serde::from_value::<IdfLoader>(value).ok())
            .unwrap_or_default();
        Box::new(options)
    }
}

/// A finite list of all the errors that can occur when flashing a given file.
///
/// This includes corrupt file issues,
/// OS permission issues as well as chip connectivity and memory boundary issues.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum EspIdfFormatError {
    /// Failed to format as esp-idf binary
    Idf(#[from] espflash::Error),

    /// Target {0} does not support the esp-idf format
    IdfUnsupported(String),

    /// Could not determine flash size.
    FlashSizeDetection(#[source] FlashError),

    /// Could not read the chip revision.
    ChipRevisionDetection(#[source] FlashError),
}

/// Loads an ELF file as an esp-idf application into the loader by converting the main application
/// to the esp-idf bootloader format, appending it to the loader along with the bootloader and
/// partition table.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct IdfLoader {
    /// The bootloader
    pub bootloader: Option<PathBuf>,
    /// The partition table
    pub partition_table: Option<PathBuf>,
    /// The target app partition
    pub target_app_partition: Option<String>,
    /// Flash SPI mode
    pub flash_mode: Option<FlashMode>,
    /// Flash SPI frequency
    pub flash_frequency: Option<FlashFrequency>,
}

impl ImageLoader for IdfLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        context: &mut ImageContext<'_>,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut elf_buffer = Vec::new();
        file.read_to_end(&mut elf_buffer)?;

        if context.metadata(&elf_buffer).is_some_and(is_direct_boot) {
            tracing::info!("The image boots without a 2nd-stage bootloader, flashing it as ELF.");
            self.warn_unused_options();
            return ElfLoader(ElfOptions::default()).load(
                flash_loader,
                context,
                &mut Cursor::new(elf_buffer),
            );
        }

        self.load_idf(flash_loader, context, &elf_buffer)
    }
}

/// Returns `true` if the ROM starts the image with `metadata` directly, without the ESP-IDF
/// 2nd-stage bootloader. `esp-bootloader-direct-boot` marks these images.
fn is_direct_boot(metadata: &ElfMetadata) -> bool {
    metadata
        .get(&keys::BOOTLOADER_NAME)
        .is_some_and(|name| name == "DIRECT-BOOT")
}

impl IdfLoader {
    /// Warns about the options that a direct-boot image does not use.
    fn warn_unused_options(&self) {
        let options = [
            ("bootloader", self.bootloader.is_some()),
            ("partition table", self.partition_table.is_some()),
            ("target app partition", self.target_app_partition.is_some()),
            ("flash mode", self.flash_mode.is_some()),
            ("flash frequency", self.flash_frequency.is_some()),
        ];
        for (name, _) in options.iter().filter(|(_, set)| *set) {
            tracing::warn!("A direct-boot image has no ESP-IDF {name}, ignoring the option.");
        }
    }

    /// Converts the ELF image `elf_buffer` to the ESP-IDF format, and adds it to the loader along
    /// with the bootloader and the partition table.
    fn load_idf(
        &self,
        flash_loader: &mut FlashLoader,
        context: &mut ImageContext<'_>,
        elf_buffer: &Vec<u8>,
    ) -> Result<(), FileDownloadError> {
        let session = &mut *context.session;
        session
            .detect_memory_map()
            .map_err(EspIdfFormatError::FlashSizeDetection)
            .map_err(|e| into_format_error("esp-idf", e))?;

        let target = session.target();
        let target_name = target
            .name
            .split_once('-')
            .map(|(name, _)| name)
            .unwrap_or(target.name.as_str());
        let chip = espflash::target::Chip::from_str(target_name)
            .map_err(|_| EspIdfFormatError::IdfUnsupported(target.name.to_string()))
            .map_err(|e| into_format_error("esp-idf", e))?;

        let mut algo = Flasher::new(target, 0, &target.flash_algorithms[0])
            .map_err(FileDownloadError::Flash)?;

        session
            .core(0)
            .unwrap()
            .reset_and_halt(Duration::from_millis(500))
            .map_err(FlashError::ResetAndHalt)
            .map_err(EspIdfFormatError::ChipRevisionDetection)
            .map_err(|e| into_format_error("esp-idf", e))?;

        let mut chip_revision = 0;
        algo.run_verify(session, &mut FlashProgress::empty(), |flasher, _| {
            if flasher.has_vendor_function("ChipRevision") {
                tracing::debug!("Reading chip revision.");
                let result = flasher.call_vendor_function("ChipRevision", [None; 4])?;
                if result < u16::MAX as u32 {
                    chip_revision = result as u16;
                } else {
                    tracing::warn!("Unexpected chip revision value: {result}. Defaulting to 0.");
                }
            }
            Ok(())
        })
        .map_err(EspIdfFormatError::ChipRevisionDetection)
        .map_err(|e| into_format_error("esp-idf", e))?;

        let flash_size = flash_size(session.target());

        tracing::info!("Detected flash size: {:?}", flash_size);

        let flash_data = FlashData::new(
            {
                let mut settings = FlashSettings::default();

                settings.size = flash_size;
                settings.freq = self.flash_frequency;
                settings.mode = self.flash_mode;

                settings
            },
            chip_revision,
            None,
            chip,
            // TODO: auto-detect the crystal frequency.
            chip.default_xtal_frequency(),
        );

        check_idf_bootloader(elf_buffer)
            .map_err(|e| {
                EspIdfFormatError::Idf(espflash::Error::AppDescriptorNotPresent(e.to_string()))
            })
            .map_err(|e| into_format_error("esp-idf", e))?;
        context.check_image_chip(elf_buffer)?;

        let image = IdfBootloaderFormat::new(
            elf_buffer,
            &flash_data,
            self.partition_table.as_deref(),
            self.bootloader.as_deref(),
            None,
            self.target_app_partition.as_deref(),
        )
        .map_err(EspIdfFormatError::Idf)
        .map_err(|e| into_format_error("esp-idf", e))?;

        for data in image.flash_segments() {
            flash_loader.add_data(data.addr.into(), &data.data)?;
        }

        Ok(())
    }
}

/// Returns the size of the target's flash region, if the image header can express it.
fn flash_size(target: &Target) -> Option<FlashSize> {
    let region_size = target
        .memory_map
        .iter()
        .filter_map(MemoryRegion::as_nvm_region)
        .find(|region| !region.is_alias)
        .map_or(0, |region| region.range.end - region.range.start);

    match region_size {
        0x40000 => Some(FlashSize::_256Kb),
        0x80000 => Some(FlashSize::_512Kb),
        0x100000 => Some(FlashSize::_1Mb),
        0x200000 => Some(FlashSize::_2Mb),
        0x400000 => Some(FlashSize::_4Mb),
        0x800000 => Some(FlashSize::_8Mb),
        0x1000000 => Some(FlashSize::_16Mb),
        0x2000000 => Some(FlashSize::_32Mb),
        0x4000000 => Some(FlashSize::_64Mb),
        0x8000000 => Some(FlashSize::_128Mb),
        0x10000000 => Some(FlashSize::_256Mb),
        _ => None,
    }
}
