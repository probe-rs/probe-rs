use std::collections::HashMap;

use probe_rs_target::{MemoryRange, MemoryRegion, NvmRegion};

use crate::flashing::{flasher::Flasher, FlashError, FlashLoader};
use crate::flashing::{FlashAlgorithm, FlashLayout, FlashSector};
use crate::Session;

use super::FlashProgress;

/// Mass-erase all nonvolatile memory.
///
/// The optional progress will only be used to emit RTT messages.
/// No actual indication for the state of the erase all operation will be given.
pub fn erase_all(session: &mut Session, progress: FlashProgress) -> Result<(), FlashError> {
    tracing::debug!("Erasing all...");

    let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();
    tracing::debug!("Regions:");
    for region in session
        .target()
        .memory_map
        .iter()
        .filter_map(MemoryRegion::as_nvm_region)
    {
        if region.is_alias {
            tracing::debug!("Skipping alias memory region {:#010x?}", region.range);
            continue;
        }
        tracing::debug!(
            "    region: {:#010x?} ({} bytes)",
            region.range,
            region.range.end - region.range.start
        );

        let algo = FlashLoader::get_flash_algorithm_for_region(region, session.target())?;

        // Get the first core that can access the region
        let core_name = region
            .cores
            .first()
            .ok_or_else(|| FlashError::NoNvmCoreAccess(region.clone()))?;

        let entry = algos
            .entry((algo.name.clone(), core_name.clone()))
            .or_default();
        entry.push(region.clone());

        tracing::debug!("     -- using algorithm: {}", algo.name);
    }

    // No longer needs to be mutable.
    let algos = algos;

    let mut do_chip_erase = true;

    let mut phases = vec![];

    // Walk through the algos to create a layout of the flash.
    for ((algo_name, core), regions) in algos.iter() {
        // This can't fail, algo_name comes from the target.
        let algo = session.target().flash_algorithm_by_name(algo_name);
        let algo = algo.unwrap().clone();

        let flash_algorithm =
            FlashAlgorithm::assemble_from_raw_with_core(&algo, core, session.target())?;

        let chip_erase_supported =
            session.has_sequence_erase_all() || flash_algorithm.pc_erase_all.is_some();
        // If the first flash algo doesn't support erase all, disable chip erase.
        // TODO: we could sort by support but it's unlikely to make a difference.
        if do_chip_erase && !chip_erase_supported {
            do_chip_erase = false;
        }

        let mut layout = FlashLayout::default();

        for region in regions {
            for info in flash_algorithm.iter_sectors() {
                let range = info.address_range();

                if region.range.contains_range(&range) {
                    layout.sectors.push(FlashSector {
                        address: info.base_address,
                        size: info.size,
                    });
                }
            }
        }
        phases.push(layout);
    }

    progress.initialized(do_chip_erase, false, phases);

    for ((algo_name, core_name), regions) in algos {
        tracing::debug!("Erasing with algorithm: {}", algo_name);

        // This can't fail, algo_name comes from the target.
        let algo = session.target().flash_algorithm_by_name(&algo_name);
        let algo = algo.unwrap().clone();

        let core_index = session.target().core_index_by_name(&core_name).unwrap();
        let mut flasher = Flasher::new(session, core_index, &algo)?;

        if flasher.is_chip_erase_supported(session) {
            tracing::debug!("     -- chip erase supported, doing it.");
            flasher.run_erase_all(session, &progress)?;
        } else {
            tracing::debug!("     -- chip erase not supported, erasing by sector.");

            // loop over all sectors erasing them individually instead.

            let sectors = flasher
                .flash_algorithm()
                .iter_sectors()
                .filter(|info| {
                    let range = info.base_address..info.base_address + info.size;
                    regions.iter().any(|r| r.range.contains_range(&range))
                })
                .collect::<Vec<_>>();

            flasher.run_erase(session, &progress, |active| {
                for info in sectors {
                    tracing::debug!(
                        "    sector: {:#010x}-{:#010x} ({} bytes)",
                        info.base_address,
                        info.base_address + info.size,
                        info.size
                    );
                    let sector = FlashSector {
                        address: info.base_address,
                        size: info.size,
                    };

                    active.erase_sector(&sector)?;
                }
                Ok(())
            })?;
        }
    }

    Ok(())
}

/// Erases `sectors` sectors starting from `start_sector` from flash.
// TODO: currently no progress is reported by anything in this function.
pub fn erase_sectors(
    session: &mut Session,
    progress: FlashProgress,
    start_sector: usize,
    sectors: usize,
) -> Result<(), FlashError> {
    tracing::debug!(
        "Erasing sectors {start_sector} trough {}",
        start_sector + sectors
    );

    let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();
    tracing::debug!("Regions:");
    for region in session
        .target()
        .memory_map
        .iter()
        .filter_map(MemoryRegion::as_nvm_region)
    {
        if region.is_alias {
            tracing::debug!("Skipping alias memory region {:#010x?}", region.range);
            continue;
        }
        tracing::debug!(
            "    region: {:#010x?} ({} bytes)",
            region.range,
            region.range.end - region.range.start
        );

        let algo = FlashLoader::get_flash_algorithm_for_region(region, session.target())?;

        // Get the first core that can access the region
        let core_name = region
            .cores
            .first()
            .ok_or_else(|| FlashError::NoNvmCoreAccess(region.clone()))?;

        let entry = algos
            .entry((algo.name.clone(), core_name.clone()))
            .or_default();
        entry.push(region.clone());

        tracing::debug!("     -- using algorithm: {}", algo.name);
    }

    for ((algo_name, core_name), regions) in algos {
        tracing::debug!("Erasing with algorithm: {}", algo_name);

        // This can't fail, algo_name comes from the target.
        let algo = session.target().flash_algorithm_by_name(&algo_name);
        let algo = algo.unwrap().clone();

        let core_index = session.target().core_index_by_name(&core_name).unwrap();
        let mut flasher = Flasher::new(session, core_index, &algo)?;

        let sectors = flasher
            .flash_algorithm()
            .iter_sectors()
            .skip(start_sector)
            .take(sectors)
            .filter(|info| {
                let range = info.base_address..info.base_address + info.size;
                regions.iter().any(|r| r.range.contains_range(&range))
            })
            .collect::<Vec<_>>();

        flasher.run_erase(session, &progress, |active| {
            for info in sectors {
                tracing::debug!(
                    "    sector: {:#010x}-{:#010x} ({} bytes)",
                    info.base_address,
                    info.base_address + info.size,
                    info.size
                );

                let sector = FlashSector {
                    address: info.base_address,
                    size: info.size,
                };

                active.erase_sector(&sector)?;
            }
            Ok(())
        })?;
    }

    Ok(())
}
