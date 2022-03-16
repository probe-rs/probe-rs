//! Sequences for Atmel targets.

use crate::architecture::arm::ap::GenericAp;
use crate::architecture::arm::communication_interface::ArmProbeInterface;
use crate::architecture::arm::{ApAddress, ArmChipInfo, DpAddress};
use crate::config::RegistryError;
use crate::config::{self, ChipInfo};
use crate::error::Error;
use crate::Target;

const DSU_BASE_ADDR: u32 = 0x41002000;
const DID_OFFSET: u32 = 0x118; // we read from the mirrored register set, as the original set might be protected and not accessible to the debugger

const DID_MASK: u32 = 0xFFFFF0FF;

pub(crate) fn detect_target_arm(
    probe_interface: &mut Box<dyn ArmProbeInterface>,
    info: ArmChipInfo,
) -> Result<Target, Error> {
    if info.part != 0xcd0 {
        return Err(Error::ChipNotFound(RegistryError::ChipNotFound(format!(
            "Part id in rom table was '{:#x}' but expected '{:#x}' which would indicate that a DSU (Device Service Unit) is present. A DSU is mandatory to correctly identify the device.",
            info.part,
            0xcd0
        ))));
    }

    let generic_ap = GenericAp::new(ApAddress {
        dp: DpAddress::Default,
        ap: 0,
    });

    let mut memory_interface = probe_interface.memory_interface(generic_ap.into())?;

    let device_id = memory_interface.read_word_32(DSU_BASE_ADDR + DID_OFFSET)?;

    // mask revision numbers
    let masked_device_id = device_id & DID_MASK;

    config::get_target_by_chip_info_and_id(ChipInfo::Arm(info), masked_device_id)
        .map_err(|err| Error::ChipNotFound(err))
}
