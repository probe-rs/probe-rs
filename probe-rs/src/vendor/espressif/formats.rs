//! Image formats for Espressif chips.

use std::path::PathBuf;
use std::str::FromStr;

use espflash::flasher::{FlashData, FlashSettings, FlashSize};
use espflash::targets::XtalFrequency;
use serde::{Deserialize, Serialize};

use crate::flashing::{
    image::{ImageFormatDefinition, ImageLoader, ImageReader},
    FileDownloadError,
};
use crate::{config::DebugSequence, flashing::FlashLoader, Session};

/// Extended options for flashing a ESP-IDF format file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct IdfOptions {
    /// The bootloader
    pub bootloader: Option<PathBuf>,
    /// The partition table
    pub partition_table: Option<PathBuf>,
}

/// An `.elf` file that also contains an esp-idf bootloader and partition table.
#[derive(Clone)]
pub struct IdfImageFormat;

impl ImageFormatDefinition for IdfImageFormat {
    fn name(&self) -> &'static str {
        "idf"
    }

    fn default(&self) -> Box<dyn ImageLoader> {
        Box::new(IdfLoader(IdfOptions::default()))
    }
}

/// Loads an ELF file as an esp-idf application into the loader by converting the main application
/// to the esp-idf bootloader format, appending it to the loader along with the bootloader and
/// partition table.
///
/// This does not create any flash loader instructions yet.
pub struct IdfLoader(pub IdfOptions); // FIXME: unpub

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
        let chip = espflash::targets::Chip::from_str(target_name)
            .map_err(|_| FileDownloadError::IdfUnsupported(target.name.to_string()))?
            .into_target();

        // FIXME: Short-term hack until we can auto-detect the crystal frequency. ESP32 and ESP32-C2
        // have 26MHz and 40MHz options, ESP32-H2 is 32MHz, the rest is 40MHz. We need to specify
        // the frequency because different options require different bootloader images.
        let xtal_frequency = if target_name.eq_ignore_ascii_case("esp32h2") {
            XtalFrequency::_32Mhz
        } else {
            XtalFrequency::_40Mhz
        };

        let flash_size_result = session.halted_access(|session| {
            // Figure out flash size from the memory map. We need a different bootloader for each size.
            match session.target().debug_sequence.clone() {
                DebugSequence::Riscv(sequence) => sequence.detect_flash_size(session),
                DebugSequence::Xtensa(sequence) => sequence.detect_flash_size(session),
                DebugSequence::Arm(_) => panic!("There are no ARM ESP targets."),
            }
        });

        let flash_size = match flash_size_result.map_err(FileDownloadError::FlashSizeDetection)? {
            Some(0x40000) => Some(FlashSize::_256Kb),
            Some(0x80000) => Some(FlashSize::_512Kb),
            Some(0x100000) => Some(FlashSize::_1Mb),
            Some(0x200000) => Some(FlashSize::_2Mb),
            Some(0x400000) => Some(FlashSize::_4Mb),
            Some(0x800000) => Some(FlashSize::_8Mb),
            Some(0x1000000) => Some(FlashSize::_16Mb),
            Some(0x2000000) => Some(FlashSize::_32Mb),
            Some(0x4000000) => Some(FlashSize::_64Mb),
            Some(0x8000000) => Some(FlashSize::_128Mb),
            Some(0x10000000) => Some(FlashSize::_256Mb),
            _ => None,
        };

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let firmware = espflash::elf::ElfFirmwareImage::try_from(&buf[..])?;

        let flash_data = FlashData::new(
            self.0.bootloader.as_deref(),
            self.0.partition_table.as_deref(),
            None,
            None,
            {
                let mut settings = FlashSettings::default();

                settings.size = flash_size;

                settings
            },
            0,
        )?;

        let image = chip.get_flash_image(&firmware, flash_data, None, xtal_frequency)?;

        for data in image.flash_segments() {
            flash_loader.add_data(data.addr.into(), &data.data)?;
        }

        Ok(())
    }
}
