use crate::architecture::arm::ArmChipInfo;

#[derive(Debug)]
pub enum ChipInfo {
    Arm(ArmChipInfo),
}

impl From<ArmChipInfo> for ChipInfo {
    fn from(info: ArmChipInfo) -> Self {
        ChipInfo::Arm(info)
    }
}
