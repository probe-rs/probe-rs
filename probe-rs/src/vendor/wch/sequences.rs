//! WCH RISC-V debug sequence with native WCH-Link flash programming.
//!
//! For supported WCH RISC-V chips flashed through a WCH-Link probe, programming
//! bypasses the generic target-side flash algorithms entirely: the probe
//! firmware erases and programs the flash itself while probe-rs streams the
//! image in bulk packets (see `probe::wlink::flash`). With any other probe
//! the loader automatically falls back to the generic path.

use std::sync::Arc;

use probe_rs_target::{CoreType, NvmRegion};

use crate::Error;
use crate::architecture::riscv::sequences::RiscvDebugSequence;
use crate::flashing::{DebugFlashSequence, FlashLayout};
use crate::probe::wlink::WchLink;
use crate::probe::wlink::flash::{NativeFlashParams, canonical_flash_address, native_flash_params};
use crate::session::Session;

/// Debug sequence enabling native WCH-Link flashing for supported WCH RISC-V chips.
#[derive(Debug)]
pub struct WchRiscvSequence;

impl WchRiscvSequence {
    /// Create a new WCH RISC-V debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self)
    }

    /// Whether `chip_name` identifies a WCH RISC-V part with a native loader
    /// entry in its target yaml.
    ///
    /// ARM parts (CH32F) and families without loader entries (e.g. CH570/CH572)
    /// are excluded and keep using the generic target-side flash algorithms.
    pub fn is_native_flash_supported(chip_name: &str) -> bool {
        chip_name.starts_with("CH32V")
            || chip_name.starts_with("CH32M")
            || chip_name.starts_with("CH32L")
            || chip_name.starts_with("CH32H")
            || chip_name.starts_with("CH32X")
            || chip_name.starts_with("CH641")
            || chip_name.starts_with("CH643")
            || chip_name.starts_with("CH645")
    }

    /// Whether the chip is a RISC-V part eligible for native flashing.
    pub fn supports_chip(cores: &[probe_rs_target::Core], chip_name: &str) -> bool {
        cores.iter().any(|core| core.core_type == CoreType::Riscv)
            && Self::is_native_flash_supported(chip_name)
    }
}

impl RiscvDebugSequence for WchRiscvSequence {
    fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        Some(Arc::new(WchNativeFlashSequence))
    }
}

/// Host-side flash programming through native WCH-Link probe commands.
///
/// Implements [`DebugFlashSequence`] on top of the probe-firmware
/// `fastprogram` flow. Erase is always a chip erase (the probe offers no
/// sector erase); programming streams whole contiguous runs per region.
#[derive(Debug)]
pub struct WchNativeFlashSequence;

/// Get the WCH-Link probe behind `session`.
fn wch_link(session: &mut Session) -> Result<&mut WchLink, Error> {
    session
        .jtag_probe_mut()
        .and_then(|probe| probe.try_into::<WchLink>())
        .ok_or_else(|| {
            Error::Other("WCH native flashing requires a WCH-Link probe in RV mode".to_string())
        })
}

/// Resolve the native protocol parameters for the attached chip family.
fn native_params(session: &mut Session) -> Result<NativeFlashParams, Error> {
    let family = wch_link(session)?.chip_family();
    native_flash_params(family).ok_or_else(|| {
        Error::Other(format!(
            "WCH-Link native flashing is not supported for chip family {family:?}"
        ))
    })
}

/// Resolve the native protocol parameters and loader blob for the attached chip.
///
/// Parameters come from the probe-reported chip family; the blob bytes come
/// from the target yaml's `*-usr-native` flash algorithm entry.
fn native_loader(session: &mut Session) -> Result<(NativeFlashParams, Vec<u8>), Error> {
    let params = native_params(session)?;
    let blob = session
        .target()
        .flash_algorithm_by_name(params.algo_name)
        .map(|algo| algo.instructions.clone())
        .ok_or_else(|| {
            Error::Other(format!(
                "Target {} does not provide the native loader entry {}",
                session.target().name,
                params.algo_name,
            ))
        })?;
    Ok((params, blob))
}

fn address_u32(address: u64) -> Result<u32, Error> {
    u32::try_from(address).map_err(|_| {
        Error::Other(format!(
            "WCH native flashing does not support addresses above 4 GiB (got {address:#x})"
        ))
    })
}

/// Group `(address, bytes)` pairs into contiguous `(start address, bytes)`
/// runs so each run can be streamed with a single native program operation.
fn contiguous_runs(pages: &[(u64, &[u8])]) -> Vec<(u64, Vec<u8>)> {
    let mut sorted: Vec<_> = pages.iter().collect();
    sorted.sort_by_key(|(address, _)| *address);

    let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
    for (address, data) in sorted {
        let contiguous = runs
            .last()
            .is_some_and(|(start, image)| *start + image.len() as u64 == *address);
        if contiguous {
            runs.last_mut()
                .expect("contiguity was just checked")
                .1
                .extend_from_slice(data);
        } else {
            runs.push((*address, data.to_vec()));
        }
    }
    runs
}

/// End of the code-flash window on all natively supported families
/// (canonical `0x0800_0000` window plus the zero alias, the same split
/// `wlink`'s address fixup assumes). System flash, option bytes and other
/// regions live above it and keep their generic algorithms.
const CODE_FLASH_END: u64 = 0x1000_0000;

impl DebugFlashSequence for WchNativeFlashSequence {
    fn supports_region(&self, region: &NvmRegion) -> bool {
        region.range.end <= CODE_FLASH_END
    }

    fn supports_keep_unwritten_bytes(&self) -> bool {
        // Native programming always mass-erases code flash without
        // readback; preservation must go through the generic path.
        false
    }

    fn supports_probe(&self, session: &mut Session) -> bool {
        // Escape hatch: setting `PROBE_RS_WCH_NATIVE_FLASH=0` restores the
        // generic target-side flash algorithms (same env-var mechanism as
        // `PROBE_RS_PREFER_FLASH_ALGO`).
        if std::env::var("PROBE_RS_WCH_NATIVE_FLASH").as_deref() == Ok("0") {
            tracing::info!("WCH native flashing disabled by environment");
            return false;
        }
        // Native flashing needs a WCH-Link probe with bulk data endpoints
        // *and* protocol parameters for the attached chip family, otherwise
        // fall back to generic target-side flashing instead of failing at
        // program time.
        let supported = wch_link(session).ok().and_then(|wlink| {
            wlink
                .has_native_data_endpoints()
                .then(|| native_flash_params(wlink.chip_family()))
                .flatten()
        });
        supported.is_some_and(|params| {
            session
                .target()
                .flash_algorithm_by_name(params.algo_name)
                .is_some()
        })
    }

    fn prepare_flash(&self, session: &mut Session) -> Result<(), Error> {
        wch_link(session)?.native_unprotect_flash()?;
        Ok(())
    }

    fn erase_all(&self, session: &mut Session) -> Result<(), Error> {
        wch_link(session)?.native_erase_flash()?;
        Ok(())
    }

    fn supports_sector_erase(&self) -> bool {
        false
    }

    fn program(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<(), Error> {
        // Single-shot fallback; the loader prefers `program_image` below, which
        // amortises the loader-blob handshake over whole contiguous runs.
        let (params, blob) = native_loader(session)?;
        let address = canonical_flash_address(&params, address_u32(address)?);
        wch_link(session)?.native_write_flash(address, data, &blob, &params)?;
        Ok(())
    }

    fn program_image(
        &self,
        session: &mut Session,
        regions: &[(&NvmRegion, &FlashLayout)],
    ) -> Result<bool, Error> {
        let (params, blob) = native_loader(session)?;
        let wlink = wch_link(session)?;
        for (region, layout) in regions {
            let pages: Vec<(u64, &[u8])> = layout
                .pages()
                .iter()
                .map(|page| (page.address(), page.data()))
                .collect();
            for (start, image) in contiguous_runs(&pages) {
                tracing::info!(
                    "WCH-Link: programming {} bytes at {:#010x} (region {:?})",
                    image.len(),
                    start,
                    region.name,
                );
                let start = canonical_flash_address(&params, address_u32(start)?);
                wlink.native_write_flash(start, &image, &blob, &params)?;
            }
        }
        Ok(true)
    }

    fn verify(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<bool, Error> {
        let params = native_params(session)?;
        let address = canonical_flash_address(&params, address_u32(address)?);
        let len = u32::try_from(data.len()).map_err(|_| {
            Error::Other(format!(
                "WCH-Link native verify does not support lengths above 4 GiB (got {} bytes)",
                data.len(),
            ))
        })?;
        let read_back = wch_link(session)?.native_read_memory(address, len)?;
        Ok(read_back == data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_flash_name_matching_covers_supported_families_only() {
        for name in [
            "CH32V307VCT6",
            "CH32M007G8R6",
            "CH32L103C8T6",
            "CH32H417QEU6",
            "CH32X035F8U6",
            "CH641F",
            "CH643W",
            "CH645F",
        ] {
            assert!(
                WchRiscvSequence::is_native_flash_supported(name),
                "{name} should use native flashing"
            );
        }

        for name in ["CH32F103C8T6", "CH570", "CH572", "STM32F103C8T6"] {
            assert!(
                !WchRiscvSequence::is_native_flash_supported(name),
                "{name} should use generic flashing"
            );
        }
    }

    #[test]
    fn contiguous_runs_sorts_merges_and_splits_pages() {
        let first = [0x01u8];
        let middle = [0x02u8, 0x03];
        let last = [0x04u8];
        let separate = [0xAAu8];

        let runs = contiguous_runs(&[
            (0x0800_0100, &separate[..]),
            (0x0800_0003, &last[..]),
            (0x0800_0000, &first[..]),
            (0x0800_0001, &middle[..]),
        ]);

        assert_eq!(
            runs,
            vec![
                (0x0800_0000, vec![0x01, 0x02, 0x03, 0x04]),
                (0x0800_0100, vec![0xAA]),
            ]
        );
    }

    #[test]
    fn native_sequence_only_covers_erasable_code_flash() {
        let sequence = WchNativeFlashSequence;
        let region = |start: u64, end: u64| NvmRegion {
            name: None,
            range: start..end,
            cores: vec!["main".to_string()],
            is_alias: false,
            access: None,
        };

        assert!(sequence.supports_region(&region(0x0800_0000, 0x0807_8000)));
        assert!(sequence.supports_region(&region(0x0000_0000, 0x0007_8000)));
        assert!(!sequence.supports_region(&region(0x1fff_8000, 0x1fff_f000)));
        assert!(!sequence.supports_keep_unwritten_bytes());
    }

    #[cfg(feature = "builtin-targets")]
    #[test]
    fn every_supported_builtin_chip_has_one_inert_native_loader() {
        use crate::config::Registry;

        let registry = Registry::from_builtin_families();
        let mut checked = 0;
        for family in registry.families() {
            for chip in family.variants() {
                if !WchRiscvSequence::supports_chip(&chip.cores, &chip.name) {
                    continue;
                }

                let target = registry
                    .get_target_by_name(&chip.name)
                    .unwrap_or_else(|e| panic!("{} is not a built-in target: {e}", chip.name));
                let natives: Vec<_> = target
                    .flash_algorithms
                    .iter()
                    .filter(|algo| algo.name.ends_with("-usr-native"))
                    .collect();

                assert_eq!(
                    natives.len(),
                    1,
                    "{} must list exactly one native loader entry",
                    chip.name
                );
                let native = natives[0];
                assert!(!native.default, "{} entry must not be a default", chip.name);
                assert_eq!(
                    native.flash_properties.address_range,
                    0..0,
                    "{} entry must have an empty address range",
                    chip.name
                );
                assert!(
                    !native.instructions.is_empty(),
                    "{} entry must carry the loader blob",
                    chip.name
                );
                checked += 1;
            }
        }

        assert_ne!(checked, 0, "expected at least one supported built-in chip");
    }
}
