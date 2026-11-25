//! Microchip vendor support.

use std::{thread, time::Duration};

use bitfield::bitfield;
use jep106::JEP106Code;
use probe_rs_target::{Chip, chip_detection::ChipDetectionMethod};

use crate::{
    Error,
    architecture::arm::{
        ArmChipInfo, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        memory::ArmMemoryInterface,
    },
    config::{DebugSequence, Registry},
    vendor::{
        Vendor,
        microchip::sequences::{
            atsam::{AtSAM, DsuDid},
            mec17xx::Mec172x,
            pic32cm_pl10::Pic32cmPl10,
        },
    },
};

pub mod sequences;

const JEP_ATMEL: JEP106Code = JEP106Code::new(0x0, 0x1f);
const JEP_MICROCHIP: JEP106Code = JEP106Code::new(0x0, 0x29);

bitfield! {
    /// DSU Device Identification register with the PNMID/PNDID layout, as found on the PIC32CM PL10.
    #[derive(Copy, Clone, PartialEq, Eq)]
    struct MicrochipDsuDid(u32);
    impl Debug;

    /// Version code.
    ver, _ : 31, 28;

    /// Part Number Mask ID.
    pnmid, _ : 27, 20;

    /// Part Number Device ID.
    pndid, _ : 19, 12;

    /// JEDEC manufacturer ID.
    manid, _ : 11, 1;

    /// Fixed value of 1.
    fv, _ : 0;
}

fn is_pic32cm_pl10(name: &str) -> bool {
    name.starts_with("PIC32CM") && name.get(11..13) == Some("PL")
}

/// Read a DSU DID register.
///
/// The Boot ROM of some devices resets them in a loop when there is no application, and reads
/// during a reset, which lasts a few milliseconds, return 0. Those are retried after a while.
fn read_dsu_did(
    memory: &mut dyn ArmMemoryInterface,
    address: u32,
) -> Result<MicrochipDsuDid, ArmError> {
    let mut value = memory.read_word_32(address.into())?;
    for _ in 0..5 {
        if value != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
        value = memory.read_word_32(address.into())?;
    }
    Ok(MicrochipDsuDid(value))
}

/// Identify devices whose DSU has a family-specific DID address, with the PNMID/PNDID layout.
fn detect_by_dsu_did(
    registry: &Registry,
    interface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
) -> Result<Option<String>, Error> {
    let mut memory = interface.memory_interface(ap)?;

    // These are tried before the fixed ATSAM DID address, which may hold a different register
    // on these devices.
    let mut did_cache: Vec<(u32, Option<MicrochipDsuDid>)> = vec![];
    for family in registry.families() {
        for info in family
            .chip_detection
            .iter()
            .filter_map(ChipDetectionMethod::as_microchip_dsu_did)
        {
            let did = match did_cache.iter().find(|(a, _)| *a == info.did_address) {
                Some((_, did)) => *did,
                None => {
                    let did = match read_dsu_did(&mut *memory, info.did_address) {
                        Ok(did) => Some(did),
                        Err(error) => {
                            tracing::debug!(
                                "Failed to read DSU DID at {:#010x}: {error}",
                                info.did_address
                            );
                            None
                        }
                    };
                    did_cache.push((info.did_address, did));
                    did
                }
            };

            let Some(did) = did else { continue };
            if !did.fv() || info.pnmid != did.pnmid() as u8 {
                continue;
            }
            if let Some(variant) = info.variants.get(&(did.pndid() as u8)) {
                return Ok(Some(variant.clone()));
            }
        }
    }

    Ok(None)
}

/// Microchip
#[derive(docsplay::Display)]
pub struct Microchip;

impl Vendor for Microchip {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("ATSAMD1")
            || chip.name.starts_with("ATSAMD2")
            || chip.name.starts_with("ATSAMDA")
            || chip.name.starts_with("ATSAMD5")
            || chip.name.starts_with("ATSAME5")
            || chip.name.starts_with("PIC32CX")
        {
            DebugSequence::Arm(AtSAM::create())
        } else if is_pic32cm_pl10(&chip.name) {
            DebugSequence::Arm(Pic32cmPl10::create())
        } else if chip.name.starts_with("MEC172") {
            DebugSequence::Arm(Mec172x::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    fn try_detect_arm_chip(
        &self,
        registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if ![JEP_ATMEL, JEP_MICROCHIP].contains(&chip_info.manufacturer) || chip_info.part != 0xCD0
        {
            return Ok(None);
        }

        // FIXME: This is a bit shaky but good enough for now.
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);

        if let Some(variant) = detect_by_dsu_did(registry, interface, access_port)? {
            return Ok(Some(variant));
        }

        let mut memory = interface.memory_interface(access_port)?;

        // This device has an Atmel DSU - Read and parse the DSU DID register
        let did = DsuDid(memory.read_word_32(DsuDid::ADDRESS)?);

        for family in registry.families() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_atsam_dsu)
            {
                if info.processor != did.processor() as u8
                    || info.family != did.family() as u8
                    || info.series != did.series() as u8
                {
                    continue;
                }
                for (devsel, variant) in info.variants.iter() {
                    if *devsel == did.devsel() as u8 {
                        return Ok(Some(variant.clone()));
                    }
                }
            }
        }

        Ok(None)
    }
}
