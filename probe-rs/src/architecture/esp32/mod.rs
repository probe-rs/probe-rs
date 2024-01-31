//! Common code for ESP32 chips.

use espflash::flasher::FlashSize;

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
}
