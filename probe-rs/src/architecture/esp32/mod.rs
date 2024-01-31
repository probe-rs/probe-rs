//! Common code for ESP32 chips.

use espflash::flasher::FlashSize;
use espflash::targets::XtalFrequency;

use crate::Error;

/// Debug sequences for various ESP32 chips.
pub trait EspDebugSequence {
    /// The communication interface required by the chip.
    type Interface;

    /// Detects the flash size of the target.
    fn detect_flash_size(
        &self,
        _interface: &mut Self::Interface,
    ) -> Result<Option<FlashSize>, Error> {
        Ok(None)
    }

    /// Detects the crystal frequency of the target.
    fn detect_xtal_frequency(
        &self,
        _interface: &mut Self::Interface,
    ) -> Result<XtalFrequency, Error> {
        // For now we just return the most common frequency.
        Ok(XtalFrequency::_40Mhz)
    }
}
