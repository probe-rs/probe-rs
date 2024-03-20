use std::collections::HashMap;

use probe_rs_target::{MemoryRange, MemoryRegion, NvmRegion};

use crate::flashing::{flasher::Flasher, FlashError, FlashLoader};
use crate::Session;

use super::FlashProgress;

/// Mass-erase all nonvolatile memory.
///
/// The optional progress will only be used to emit RTT messages.
/// No actual indication for the state of the erase all operation will be given.
pub fn erase_all(session: &mut Session, progress: Option<FlashProgress>) -> Result<(), FlashError> {
    tracing::debug!("Erasing all...");

    let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();
    tracing::debug!("Regions:");
    for region in &session.target().memory_map {
        if let MemoryRegion::Nvm(region) = region {
            if region.is_alias {
                tracing::debug!(
                    "Skipping alias memory region {:#010X}..{:#010X}",
                    region.range.start,
                    region.range.end
                );
                continue;
            }
            tracing::debug!(
                "    region: {:#010X}..{:#010X} ({} bytes)",
                region.range.start,
                region.range.end,
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
    }

    for ((algo_name, core_name), regions) in algos {
        tracing::debug!("Erasing with algorithm: {}", algo_name);

        // This can't fail, algo_name comes from the target.
        let algo = session.target().flash_algorithm_by_name(&algo_name);
        let algo = algo.unwrap().clone();

        let core_index = session.target().core_index_by_name(&core_name).unwrap();
        let mut flasher = Flasher::new(session, core_index, &algo, progress.clone())?;

        if flasher.is_chip_erase_supported() {
            tracing::debug!("     -- chip erase supported, doing it.");
            flasher.run_erase_all()?;
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

            flasher.run_erase(|active| {
                for info in sectors {
                    tracing::debug!(
                        "    sector: {:08x}-{:08x} ({} bytes)",
                        info.base_address,
                        info.base_address + info.size,
                        info.size
                    );

                    active.erase_sector(info.base_address)?;
                }
                Ok(())
            })?;
        }
    }

    Ok(())
}

/// Erases `sectors` sectors starting from `start_sector` from flash.
pub fn erase_sectors(
    session: &mut Session,
    progress: Option<FlashProgress>,
    start_sector: usize,
    sectors: usize,
) -> Result<(), FlashError> {
    tracing::debug!(
        "Erasing sectors {start_sector} trough {}",
        start_sector + sectors
    );

    let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();
    tracing::debug!("Regions:");
    for region in &session.target().memory_map {
        if let MemoryRegion::Nvm(region) = region {
            if region.is_alias {
                tracing::debug!(
                    "Skipping alias memory region {:#010X}..{:#010X}",
                    region.range.start,
                    region.range.end
                );
                continue;
            }
            tracing::debug!(
                "    region: {:08x}-{:08x} ({} bytes)",
                region.range.start,
                region.range.end,
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
    }

    for ((algo_name, core_name), regions) in algos {
        tracing::debug!("Erasing with algorithm: {}", algo_name);

        // This can't fail, algo_name comes from the target.
        let algo = session.target().flash_algorithm_by_name(&algo_name);
        let algo = algo.unwrap().clone();

        let core_index = session.target().core_index_by_name(&core_name).unwrap();
        let mut flasher = Flasher::new(session, core_index, &algo, progress.clone())?;

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

        flasher.run_erase(|active| {
            for info in sectors {
                tracing::debug!(
                    "    sector: {:08x}-{:08x} ({} bytes)",
                    info.base_address,
                    info.base_address + info.size,
                    info.size
                );

                active.erase_sector(info.base_address)?;
            }
            Ok(())
        })?;
    }

    Ok(())
}
