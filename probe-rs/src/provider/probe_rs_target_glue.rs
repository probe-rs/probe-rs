//! Implementations of the generic target provider/factory atop bare probe_rs_target::* types.
use super::*;
use probe_rs_target::{Chip, ChipFamily};

impl<'a> Provider for &'a ChipFamily {
    fn name(&self) -> &str {
        &self.name
    }

    fn families(&self) -> Box<dyn Iterator<Item = Box<dyn Family + '_>> + '_> {
        Box::new(std::iter::once({
            let family: Box<dyn Family + '_> = Box::new(*self);
            family
        }))
    }

    fn autodetect_arm(
        &self,
        chip_info: &ArmChipInfo,
        _memory: &mut Memory,
    ) -> Option<Box<dyn Variant + '_>> {
        if self.manufacturer != Some(chip_info.manufacturer) {
            return None;
        }

        let matching_chips: Vec<_> = self
            .variants
            .iter()
            .filter(|v| v.part.map(|p| p == chip_info.part).unwrap_or(false))
            .collect();

        if matching_chips.len() == 1 {
            let chip = matching_chips.into_iter().next().unwrap();
            Some(Box::new(ChipInFamily(*self, chip)))
        } else {
            tracing::debug!(
                "Found {} matching chips for information {:?}, unable to determine chip",
                matching_chips.len(),
                chip_info
            );
            None
        }
    }
}

impl<'a> Family<'a> for &'a ChipFamily {
    fn name(&self) -> &'a str {
        &self.name
    }

    fn variants(&self) -> Box<dyn Iterator<Item = Box<dyn Variant<'a> + 'a>> + 'a> {
        Box::new(self.variants.iter().map(|chip| {
            let target: Box<dyn Variant + '_> = Box::new(ChipInFamily(self, chip));
            target
        }))
    }
}

pub(crate) struct ChipInFamily<'a>(pub &'a ChipFamily, pub &'a Chip);

impl<'a> ChipInFamily<'a> {
    fn debug_sequence(&self) -> DebugSequence {
        use crate::architecture::arm::sequences::{
            atsame5x::AtSAME5x,
            infineon::XMC4000,
            nrf52::Nrf52,
            nrf53::Nrf5340,
            nrf91::Nrf9160,
            nxp::{MIMXRT10xx, LPC55S69},
            stm32f_series::Stm32fSeries,
            stm32h7::Stm32h7,
            DefaultArmSequence,
        };
        use crate::architecture::riscv::sequences::esp32c3::ESP32C3;
        use crate::architecture::riscv::sequences::DefaultRiscvSequence;
        use probe_rs_target::Architecture;

        let chip = self.1;

        // We always just take the architecture of the first core which is okay if there is no mixed architectures.
        let mut debug_sequence = match chip.cores[0].core_type.architecture() {
            Architecture::Arm => DebugSequence::Arm(DefaultArmSequence::create()),
            Architecture::Riscv => DebugSequence::Riscv(DefaultRiscvSequence::create()),
        };

        if chip.name.starts_with("MIMXRT10") {
            tracing::warn!("Using custom sequence for MIMXRT10xx");
            debug_sequence = DebugSequence::Arm(MIMXRT10xx::create());
        } else if chip.name.starts_with("LPC55S16") || chip.name.starts_with("LPC55S69") {
            tracing::warn!("Using custom sequence for LPC55S16/LPC55S69");
            debug_sequence = DebugSequence::Arm(LPC55S69::create());
        } else if chip.name.starts_with("esp32c3") {
            tracing::warn!("Using custom sequence for ESP32c3");
            debug_sequence = DebugSequence::Riscv(ESP32C3::create());
        } else if chip.name.starts_with("nRF5340") {
            tracing::warn!("Using custom sequence for nRF5340");
            debug_sequence = DebugSequence::Arm(Nrf5340::create());
        } else if chip.name.starts_with("nRF52") {
            tracing::warn!("Using custom sequence for nRF52");
            debug_sequence = DebugSequence::Arm(Nrf52::create());
        } else if chip.name.starts_with("nRF9160") {
            tracing::warn!("Using custom sequence for nRF9160");
            debug_sequence = DebugSequence::Arm(Nrf9160::create());
        } else if chip.name.starts_with("STM32H7") {
            tracing::warn!("Using custom sequence for STM32H7");
            debug_sequence = DebugSequence::Arm(Stm32h7::create());
        } else if chip.name.starts_with("STM32F2")
            || chip.name.starts_with("STM32F4")
            || chip.name.starts_with("STM32F7")
        {
            tracing::warn!("Using custom sequence for STM32F2/4/7");
            debug_sequence = DebugSequence::Arm(Stm32fSeries::create());
        } else if chip.name.starts_with("ATSAMD5") || chip.name.starts_with("ATSAME5") {
            tracing::warn!("Using custom sequence for {}", chip.name);
            debug_sequence = DebugSequence::Arm(AtSAME5x::create());
        } else if chip.name.starts_with("XMC4") {
            tracing::warn!("Using custom sequence for XMC4000");
            debug_sequence = DebugSequence::Arm(XMC4000::create());
        };

        debug_sequence
    }
}

impl<'a> Variant<'a> for ChipInFamily<'a> {
    fn name(&self) -> &'a str {
        &self.1.name
    }

    fn to_target(&self) -> Target {
        let flash_algorithms = self
            .0
            .flash_algorithms
            .iter()
            .filter(|a| self.1.flash_algorithms.contains(&a.name))
            .cloned()
            .collect();

        Target {
            name: self.1.name.clone(),
            cores: self.1.cores.clone(),
            flash_algorithms,
            memory_map: self.1.memory_map.clone(),
            source: self.0.source.clone(),
            debug_sequence: self.debug_sequence(),
        }
    }
}
