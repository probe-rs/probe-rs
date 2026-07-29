use crate::architecture::arm::ArmChipInfo;

/// Information about a chip which is used
/// for automatic detection of the connected chip.
///
/// For ARM-based chips, the function [`ArmProbeInterface::read_chip_info_from_rom_table()`][r] is
/// used to read the information from the target.
///
/// [r]: crate::architecture::arm::ArmProbeInterface::read_chip_info_from_rom_table
#[derive(Debug)]
pub(crate) enum ChipInfo {
    /// ARM specific information for chip
    /// auto-detection. See [ArmChipInfo].
    Arm(ArmChipInfo),
    /// AVR specific information for chip auto-detection by device signature.
    Avr(AvrChipInfo),
}

/// AVR chip identification data used for auto-detection.
#[derive(Debug)]
pub(crate) struct AvrChipInfo {
    /// Three-byte device signature read from the production signature row.
    pub signature: [u8; 3],
}

impl From<ArmChipInfo> for ChipInfo {
    fn from(info: ArmChipInfo) -> Self {
        ChipInfo::Arm(info)
    }
}

impl From<AvrChipInfo> for ChipInfo {
    fn from(info: AvrChipInfo) -> Self {
        ChipInfo::Avr(info)
    }
}
