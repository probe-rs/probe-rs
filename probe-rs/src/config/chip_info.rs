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
}

impl From<ArmChipInfo> for ChipInfo {
    fn from(info: ArmChipInfo) -> Self {
        ChipInfo::Arm(info)
    }
}
