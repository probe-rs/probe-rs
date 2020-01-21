use crate::architecture::arm::ArmChipInfo;

#[derive(Debug)]
pub enum ChipInfo {
    Arm(ArmChipInfo),
}
