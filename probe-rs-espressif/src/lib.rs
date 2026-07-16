//! Espressif device support for probe-rs

use probe_rs::{plugin, probe::JtagAccess};

use probe_rs_target::{
    Chip, ChipFamily,
    chip_detection::{ChipDetectionMethod, EspressifDetection},
};

use probe_rs::{
    Error, MemoryInterface,
    architecture::{
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    config::{DebugSequence, Registry},
    probe::{Probe, WireProtocol},
    vendor::Vendor,
};
use sequences::{
    esp32::ESP32, esp32c2::ESP32C2, esp32c3::ESP32C3, esp32c5::ESP32C5, esp32c6::ESP32C6,
    esp32c61::ESP32C61, esp32h2::ESP32H2, esp32p4::ESP32P4, esp32s2::ESP32S2, esp32s3::ESP32S3,
    esp32s31::ESP32S31,
};

use crate::{espusbjtag::EspUsbJtagFactory, image_format::IdfLoaderFactory};

pub mod espusbjtag;
pub mod image_format;
pub mod sequences;

pub fn register_plugin() {
    let targets = targets();
    plugin::register_plugin(plugin::Plugin {
        vendors: &[&Espressif],
        image_formats: &[&IdfLoaderFactory],
        targets: &targets,
        probe_drivers: &[&EspUsbJtagFactory],
    });
}

fn targets() -> Vec<ChipFamily> {
    const ESPRESSIF_TARGETS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/targets.bincode"));

    bincode::serde::decode_from_slice(ESPRESSIF_TARGETS, bincode::config::standard())
        .expect("Failed to deserialize builtin targets. This is a bug")
        .0
}

// A magic number that resides in the ROM of Espressif chips. This points to 4 bytes that are mostly
// unique to each chip variant. There may be some overlap between revisions (e.g. esp32c3)
// and chips may be placed on modules that are configured significantly
// differently (esp32 with 1.8V or 3.3V VDD_SDIO).
// See:
// - https://github.com/esp-rs/espflash/blob/5c898ac7a37fd6ec7d7c4562585818ac878e5a2f/espflash/src/flasher/stubs.rs#L23
// - https://github.com/esp-rs/espflash/blob/5c898ac7a37fd6ec7d7c4562585818ac878e5a2f/espflash/src/flasher/mod.rs#L589-L590
const MAGIC_VALUE_ADDRESS: u64 = 0x4000_1000;

fn get_target_by_magic(info: &EspressifDetection, read_magic: u32) -> Option<String> {
    for (magic, target) in info.variants.iter() {
        if *magic == read_magic {
            return Some(target.clone());
        }
    }
    None
}

/// Identify using an established debug connection.
fn try_detect_espressif_chip(
    registry: &Registry,
    mut read_magic: impl FnMut(u64) -> Option<u32>,
    idcode: u32,
) -> Option<String> {
    tracing::debug!("Identifying chip via magic value");
    for family in registry.families() {
        for info in family
            .chip_detection
            .iter()
            .filter_map(ChipDetectionMethod::as_espressif)
        {
            if info.idcode != idcode {
                continue;
            }

            let Some(read_magic) = read_magic(MAGIC_VALUE_ADDRESS) else {
                continue;
            };
            tracing::debug!("Read magic value: {read_magic:#010x}");
            if let Some(target) = get_target_by_magic(info, read_magic) {
                return Some(target);
            }
        }
    }

    None
}

/// Espressif
#[derive(docsplay::Display)]
struct Espressif;

impl Vendor for Espressif {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.eq_ignore_ascii_case("esp32s2") {
            DebugSequence::Xtensa(ESP32S2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32s3") {
            DebugSequence::Xtensa(ESP32S3::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c2") {
            DebugSequence::Riscv(ESP32C2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c3") {
            DebugSequence::Riscv(ESP32C3::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c5") {
            DebugSequence::Riscv(ESP32C5::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c61") {
            DebugSequence::Riscv(ESP32C61::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c6") {
            DebugSequence::Riscv(ESP32C6::create())
        } else if chip.name.eq_ignore_ascii_case("esp32h2") {
            DebugSequence::Riscv(ESP32H2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32p4") {
            DebugSequence::Riscv(ESP32P4::create())
        } else if chip.name.eq_ignore_ascii_case("esp32s31") {
            DebugSequence::Riscv(ESP32S31::create())
        } else if chip.name.starts_with("esp32") {
            DebugSequence::Xtensa(ESP32::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    fn try_detect_chip_from_probe(
        &self,
        registry: &Registry,
        probe: &mut Probe,
    ) -> Result<Option<String>, Error> {
        // JTAG capability does not imply that the probe is currently using JTAG.
        if probe.protocol() != Some(WireProtocol::Jtag) {
            return Ok(None);
        }

        // Identify from JTAG IDCODE only. This works for RISC-V chips,
        // where we set a magic value of 0.
        if let Some(jtag) = probe.try_as_jtag_probe() {
            let r = identify_by_idcode(registry, jtag);

            // Ensure TAP 0 is selected before returning.
            jtag.select_target(0)?;

            r
        } else {
            Ok(None)
        }
    }

    fn try_detect_riscv_chip(
        &self,
        registry: &Registry,
        probe: &mut RiscvCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(try_detect_espressif_chip(
            registry,
            |address| {
                probe
                    .halted_access(|probe| Ok(probe.read_word_32(address).ok()))
                    .unwrap()
            },
            idcode,
        ))
    }

    fn try_detect_xtensa_chip(
        &self,
        registry: &Registry,
        probe: &mut XtensaCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(try_detect_espressif_chip(
            registry,
            |address| probe.read_word_32(address).ok(),
            idcode,
        ))
    }
}

fn identify_by_idcode(
    registry: &Registry,
    jtag: &mut dyn JtagAccess,
) -> Result<Option<String>, Error> {
    tracing::debug!("Identifying chip via JTAG IDCODE");
    use bitvec::field::BitField;
    for tap in 0..jtag.scan_chain()?.len() {
        jtag.select_target(tap)?;

        let Ok(idcode) = jtag.read_register(1, 32) else {
            return Ok(None);
        };

        let idcode = idcode.load_le::<u32>();
        tracing::debug!("JTAG IDCODE: 0x{:08x}", idcode);

        for family in registry.families() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_espressif)
            {
                if info.idcode == idcode
                    && let Some(target) = get_target_by_magic(info, 0)
                {
                    return Ok(Some(target));
                }
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use bitvec::vec::BitVec;
    use probe_rs::{
        Error,
        config::{Registry, RegistryError},
        flashing::FlashAlgorithm,
        probe::{DebugProbe, DebugProbeError, JtagAccess, JtagSequence, Probe, WireProtocol},
    };
    use probe_rs_target::ScanChainElement;

    use super::{Espressif, Vendor};

    #[derive(Debug)]
    struct ProtocolProbe {
        protocol: WireProtocol,
        scans: Arc<AtomicUsize>,
    }

    impl DebugProbe for ProtocolProbe {
        fn get_name(&self) -> &str {
            "protocol test probe"
        }

        fn speed_khz(&self) -> u32 {
            1_000
        }

        fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
            Ok(speed_khz)
        }

        fn attach(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), Error> {
            Ok(())
        }

        fn target_reset(&mut self) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
            self.protocol = protocol;
            Ok(())
        }

        fn active_protocol(&self) -> Option<WireProtocol> {
            Some(self.protocol)
        }

        fn try_as_jtag_probe(&mut self) -> Option<&mut dyn JtagAccess> {
            Some(self)
        }

        fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
            self
        }
    }

    impl JtagAccess for ProtocolProbe {
        fn set_expected_scan_chain(
            &mut self,
            _scan_chain: &[ScanChainElement],
        ) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn set_scan_chain(
            &mut self,
            _scan_chain: &[ScanChainElement],
        ) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn scan_chain(&mut self) -> Result<&[ScanChainElement], DebugProbeError> {
            self.scans.fetch_add(1, Ordering::Relaxed);
            Ok(&[])
        }

        fn shift_raw_sequence(
            &mut self,
            _sequence: JtagSequence,
        ) -> Result<BitVec, DebugProbeError> {
            unreachable!()
        }

        fn tap_reset(&mut self) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn set_idle_cycles(&mut self, _idle_cycles: u8) -> Result<(), DebugProbeError> {
            unreachable!()
        }

        fn idle_cycles(&self) -> u8 {
            unreachable!()
        }

        fn write_register(
            &mut self,
            _address: u32,
            _data: &[u8],
            _len: u32,
        ) -> Result<BitVec, DebugProbeError> {
            unreachable!()
        }

        fn write_dr(&mut self, _data: &[u8], _len: u32) -> Result<BitVec, DebugProbeError> {
            unreachable!()
        }
    }

    #[test]
    fn probe_side_detection_only_scans_active_jtag_probes() {
        let registry = Registry::new();

        for (protocol, expected_scans) in [(WireProtocol::Swd, 0), (WireProtocol::Jtag, 1)] {
            let scans = Arc::new(AtomicUsize::new(0));
            let mut probe = Probe::new(ProtocolProbe {
                protocol,
                scans: scans.clone(),
            });

            let detected = Espressif
                .try_detect_chip_from_probe(&registry, &mut probe)
                .unwrap();

            assert_eq!(detected, None);
            assert_eq!(scans.load(Ordering::Relaxed), expected_scans);
        }
    }

    #[test]
    fn validate_builtin() {
        crate::register_plugin();

        // We can't just disable the builtin targets, because of the workspace. Let's build
        // up the registry manually.
        let mut registry = Registry::new();

        // Register our targets, this validates them.
        for target in crate::targets() {
            match registry.add_target_family(target.clone()) {
                Ok(_) => {}
                Err(RegistryError::InvalidChipFamilyDefinition(_, error)) => {
                    panic!("{}: Invalid chip family definition: {}", target.name, error)
                }
                Err(other) => {
                    panic!("Unexpected error: {:?}", other)
                }
            }
        }

        for family in registry.families() {
            for target in family.variants() {
                let target = registry.get_target_by_name(&target.name).unwrap();

                for raw_flash_algo in target.flash_algorithms.iter() {
                    for core in raw_flash_algo.cores.iter() {
                        if let Err(error) = FlashAlgorithm::assemble_from_raw_with_core(
                            raw_flash_algo,
                            core,
                            &target,
                        ) {
                            panic!(
                                "Failed to initialize flash algorithm ({}, {}, {core}): {}",
                                &target.name, &raw_flash_algo.name, error
                            )
                        }
                    }
                }
            }
        }
    }
}
