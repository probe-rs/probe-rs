use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use espflash::flasher::{FlashData, FlashFrequency, FlashMode, FlashSettings, FlashSize};
use espflash::image_format::idf::{IdfBootloaderFormat, check_idf_bootloader};
use probe_rs::Session;
use probe_rs::flashing::{
    FileDownloadError, FlashError, FlashLoader, FlashProgress, Flasher, ImageFormat, ImageLoader,
    ImageReader, into_format_error,
};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

pub(crate) struct IdfLoaderFactory;

impl ImageFormat for IdfLoaderFactory {
    fn formats(&self) -> &[&str] {
        &["idf", "esp-idf", "espidf"]
    }

    fn create_loader(&self, options: Option<Value>) -> Box<dyn ImageLoader> {
        let options = options
            .and_then(|value| serde_yaml::from_value::<IdfLoader>(value).ok())
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
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
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
            .map_err(EspIdfFormatError::FlashSizeDetection)
            .map_err(|e| into_format_error("esp-idf", e))?;

        let flash_size_result = algo
            .run_verify(session, &mut FlashProgress::empty(), |flasher, _| {
                flasher.read_flash_size()
            })
            .map_err(EspIdfFormatError::FlashSizeDetection)
            .map_err(|e| into_format_error("esp-idf", e))?;

        let flash_size = match flash_size_result {
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
        };

        tracing::info!("Detected flash size: {:?}", flash_size);

        let flash_data = FlashData::new(
            {
                let mut settings = FlashSettings::default();

                settings.size = flash_size;
                settings.freq = self.flash_frequency;
                settings.mode = self.flash_mode;

                settings
            },
            0,
            None,
            chip,
            // TODO: auto-detect the crystal frequency.
            chip.default_xtal_frequency(),
        );

        let mut elf_buffer = Vec::new();
        file.read_to_end(&mut elf_buffer)?;

        check_idf_bootloader(&elf_buffer)
            .map_err(|e| {
                EspIdfFormatError::Idf(espflash::Error::AppDescriptorNotPresent(e.to_string()))
            })
            .map_err(|e| into_format_error("esp-idf", e))?;
        check_chip_compatibility_from_elf_metadata(session, &elf_buffer)?;

        let image = IdfBootloaderFormat::new(
            &elf_buffer,
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

pub fn check_chip_compatibility_from_elf_metadata(
    session: &Session,
    elf_data: &[u8],
) -> Result<(), FileDownloadError> {
    let esp_metadata = espflash::image_format::Metadata::from_bytes(Some(elf_data));

    if let Some(chip_name) = esp_metadata.chip_name() {
        let target = session.target();
        if chip_name != target.name {
            return Err(FileDownloadError::IncompatibleImageChip {
                target: target.name.clone(),
                image_chips: vec![chip_name.to_string()],
            });
        }
    }

    Ok(())
}
