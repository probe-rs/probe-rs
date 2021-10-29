use std::fs::{self};
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Error, Result};
use cmsis_pack::pdsc::{Core, Device, Package, Processor};
use cmsis_pack::{pack_index::PdscRef, utils::FromElem};
use futures::StreamExt;
use probe_rs::config::{
    Chip, ChipFamily, Core as ProbeCore, MemoryRegion, NvmRegion, RamRegion, RawFlashAlgorithm,
};
use probe_rs::{Architecture, CoreType};
use probe_rs_target::{ArmCoreAccessOptions, CoreAccessOptions, RiscvCoreAccessOptions};
use tokio::runtime::Builder;

pub(crate) enum Kind<'a, T>
where
    T: std::io::Seek + std::io::Read,
{
    Archive(&'a mut zip::ZipArchive<T>),
    Directory(&'a Path),
}

pub(crate) fn handle_package<T>(
    pdsc: Package,
    mut kind: Kind<T>,
    families: &mut Vec<ChipFamily>,
) -> Result<()>
where
    T: std::io::Seek + std::io::Read,
{
    // Forge a definition file for each device in the .pdsc file.
    let mut devices = pdsc.devices.0.into_iter().collect::<Vec<_>>();
    devices.sort_by(|a, b| a.0.cmp(&b.0));

    for (device_name, device) in devices {
        // Extract the RAM info from the .pdsc file.
        let ram = get_ram(&device);

        // Extract the flash algorithm, block & sector size and the erased byte value from the ELF binary.
        let variant_flash_algorithms = device
            .algorithms
            .iter()
            .map(|flash_algorithm| {
                let algo = match &mut kind {
                    Kind::Archive(archive) => crate::parser::extract_flash_algo(
                        archive.by_name(&flash_algorithm.file_name.as_path().to_string_lossy())?,
                        &flash_algorithm.file_name,
                        flash_algorithm.default,
                    ),
                    Kind::Directory(path) => crate::parser::extract_flash_algo(
                        std::fs::File::open(path.join(&flash_algorithm.file_name))?,
                        &flash_algorithm.file_name,
                        flash_algorithm.default,
                    ),
                }?;

                Ok(algo)
            })
            .filter_map(
                |flash_algorithm: Result<RawFlashAlgorithm>| match flash_algorithm {
                    Ok(flash_algorithm) => Some(flash_algorithm),
                    Err(error) => {
                        log::warn!("Failed to parse flash algorithm.");
                        log::warn!("Reason: {:?}", error);
                        None
                    }
                },
            )
            .collect::<Vec<_>>();

        // Extract the flash info from the .pdsc file.
        let flash = get_flash(&device);

        if device.processors.len() > 1 {
            println!("{:#?}", device.processors);
        }

        // Check if this device family is already known.
        let mut potential_family = families
            .iter_mut()
            .find(|family| family.name == device.family);

        let family = if let Some(ref mut family) = potential_family {
            family
        } else {
            families.push(ChipFamily {
                name: device.family,
                manufacturer: None,
                variants: Vec::new(),
                flash_algorithms: Vec::new(),
                source: probe_rs::config::TargetDescriptionSource::BuiltIn,
            });
            // This unwrap is always safe as we insert at least one item previously.
            families.last_mut().unwrap()
        };

        let flash_algorithm_names: Vec<_> = variant_flash_algorithms
            .iter()
            .map(|fa| fa.name.to_string())
            .collect();

        for fa in variant_flash_algorithms {
            family.flash_algorithms.push(fa);
        }

        let mut memory_map: Vec<MemoryRegion> = Vec::new();
        if let Some(mem) = ram {
            memory_map.push(MemoryRegion::Ram(mem));
        }
        if let Some(mem) = flash {
            memory_map.push(MemoryRegion::Nvm(mem));
        }

        let cores = device
            .processors
            .iter()
            .map(|processor| create_core(processor))
            .collect::<Result<Vec<_>>>()?;

        family.variants.push(Chip {
            name: device_name,
            part: None,
            cores,
            memory_map,
            flash_algorithms: flash_algorithm_names,
        });
    }

    Ok(())
}

fn create_core(processor: &Processor) -> Result<ProbeCore> {
    let core_type = core_to_probe_core(&processor.core)?;
    Ok(ProbeCore {
        name: processor
            .name
            .as_ref()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "main".to_string()),
        core_type,
        core_access_options: match core_type.architecture() {
            Architecture::Arm => CoreAccessOptions::Arm(ArmCoreAccessOptions {
                ap: processor.ap,
                psel: 0,
            }),
            Architecture::Riscv => CoreAccessOptions::Riscv(RiscvCoreAccessOptions {}),
        },
    })
}

fn core_to_probe_core(value: &Core) -> Result<CoreType, Error> {
    Ok(match value {
        Core::CortexM0 => CoreType::Armv6m,
        Core::CortexM0Plus => CoreType::Armv6m,
        Core::CortexM4 => CoreType::Armv7em,
        Core::CortexM3 => CoreType::Armv7m,
        Core::CortexM33 => CoreType::Armv8m,
        Core::CortexM7 => CoreType::Armv7em,
        c => {
            bail!("Core '{:?}' is not yet supported for target generation.", c);
        }
    })
}

// one possible implementation of walking a directory only visiting files
pub(crate) fn visit_dirs(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    // If we get a dir, look for all .pdsc files.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            visit_dirs(&entry_path, families)?;
        } else if let Some(extension) = entry_path.extension() {
            if extension == "pdsc" {
                log::info!("Found .pdsc file: {}", path.display());

                handle_package::<std::fs::File>(
                    Package::from_path(&entry.path()).map_err(|e| e.compat())?,
                    Kind::Directory(path),
                    families,
                )
                .context(format!(
                    "Failed to process .pdsc file {}.",
                    entry.path().display()
                ))?;
            }
        }
    }

    Ok(())
}

pub(crate) fn visit_file(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    log::info!("Trying to open pack file: {}.", path.display());
    // If we get a file, try to unpack it.
    let file = fs::File::open(&path)?;

    let mut archive = zip::ZipArchive::new(file)?;

    let mut pdsc_file = find_pdsc_in_archive(&mut archive)?
        .ok_or_else(|| anyhow!("Failed to find .pdsc file in archive {}", path.display()))?;

    let mut pdsc = String::new();
    pdsc_file.read_to_string(&mut pdsc)?;

    let package = Package::from_string(&pdsc).map_err(|e| {
        anyhow!(
            "Failed to parse pdsc file '{}' in CMSIS Pack {}: {}",
            pdsc_file.name(),
            path.display(),
            e
        )
    })?;

    drop(pdsc_file);

    handle_package(package, Kind::Archive(&mut archive), families)
}

pub(crate) fn visit_arm_files(families: &mut Vec<ChipFamily>) -> Result<()> {
    let packs = crate::fetch::get_vidx()?;
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let mut stream = futures::stream::iter(packs.pdsc_index.iter().enumerate().filter_map(
                |(i, pack)| {
                    if pack.deprecated.is_none() {
                        log::info!("Working PACK {}/{} ...", i, packs.pdsc_index.len());
                        Some(visit_arm_file(&pack))
                    } else {
                        log::warn!("Pack {} is deprecated. Skipping ...", pack.name);
                        None
                    }
                },
            ))
            .buffer_unordered(32);
            while let Some(result) = stream.next().await {
                families.extend(result);
            }

            Ok(())
        })
}

pub(crate) async fn visit_arm_file(pack: &PdscRef) -> Vec<ChipFamily> {
    let url = format!(
        "{url}/{vendor}.{name}.{version}.pack",
        url = pack.url,
        vendor = pack.vendor,
        name = pack.name,
        version = pack.version
    );

    log::info!("Downloading {}", url);

    let response = match reqwest::get(&url).await {
        Ok(response) => response,
        Err(error) => {
            log::error!("Failed to download pack '{}': {}", url, error);
            return vec![];
        }
    };
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            log::error!("Failed to get bytes from pack '{}': {}", url, error);
            return vec![];
        }
    };

    log::info!("Trying to open pack file: {}.", url);
    let zip = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(zip) {
        Ok(archive) => archive,
        Err(error) => {
            log::error!("Failed to open pack '{}': {}", url, error);
            return vec![];
        }
    };

    let mut pdsc_file = match find_pdsc_in_archive(&mut archive) {
        Ok(Some(file)) => file,
        Ok(None) => {
            log::error!("Failed to find .pdsc file in archive {}", &url);
            return vec![];
        }
        Err(e) => {
            log::error!("Error handling archive {}: {}", url, e);
            return vec![];
        }
    };

    let mut pdsc = String::new();

    match pdsc_file.read_to_string(&mut pdsc) {
        Ok(_) => {}
        Err(_) => {
            log::error!("Failed to read .pdsc file {}", &url);
            return vec![];
        }
    };

    let package = match Package::from_string(&pdsc) {
        Ok(package) => package,
        Err(e) => {
            log::error!(
                "Failed to parse pdsc file '{}' in CMSIS Pack {}: {}",
                pdsc_file.name(),
                &url,
                e
            );
            return vec![];
        }
    };

    drop(pdsc_file);

    let mut families = vec![];

    match handle_package(package, Kind::Archive(&mut archive), &mut families) {
        Ok(_) => {}
        Err(err) => log::error!("Something went wrong while handling pack {}: {}", url, err),
    };

    families
}

/// Extracts the pdsc out of a ZIP archive.
pub(crate) fn find_pdsc_in_archive<T>(
    archive: &mut zip::ZipArchive<T>,
) -> Result<Option<zip::read::ZipFile>>
where
    T: std::io::Seek + std::io::Read,
{
    let mut index = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let outpath = file.enclosed_name().ok_or_else(|| {
            anyhow!(
                "Error handling the ZIP file content with path '{}': Path seems to be malformed",
                file.name()
            )
        })?;

        if let Some(extension) = outpath.extension() {
            if extension == "pdsc" {
                // We cannot return the file directly here,
                // because this leads to lifetime problems.

                index = Some(i);
                break;
            }
        }
    }

    if let Some(index) = index {
        let file = archive.by_index(index)?;

        Ok(Some(file))
    } else {
        Ok(None)
    }
}

// Clippy complains about `region.range.start == cur.range.end`, but that is correct ;).
#[allow(clippy::suspicious_operation_groupings)]
pub(crate) fn get_ram(device: &Device) -> Option<RamRegion> {
    let mut regions: Vec<RamRegion> = Vec::new();
    for memory in device.memories.0.values() {
        if memory.default && memory.access.read && memory.access.write {
            regions.push(RamRegion {
                range: memory.start as u32..memory.start as u32 + memory.size as u32,
                is_boot_memory: memory.startup,
                cores: vec!["main".to_owned()],
            });
        }
    }
    if regions.len() > 1 {
        // Sort by start address
        regions.sort_by_key(|r| r.range.start);
        let mut merged: Vec<RamRegion> = Vec::new();
        let mut cur = regions.first().cloned().unwrap();
        for region in regions.iter().skip(1) {
            if region.is_boot_memory == cur.is_boot_memory && region.range.start == cur.range.end {
                // Merge with previous region
                cur.range.end = region.range.end;
            } else {
                merged.push(cur);
                cur = region.clone();
            }
        }
        merged.push(cur);
        regions = merged;

        // Sort by region size
        regions.sort_by_key(|r| r.range.end - r.range.start)
    }

    regions.last().cloned()
}

pub(crate) fn get_flash(device: &Device) -> Option<NvmRegion> {
    // Make a Vec of all memories which are flash-like
    let mut regions = Vec::new();
    for memory in device.memories.0.values() {
        if memory.default && memory.access.read && memory.access.execute && !memory.access.write {
            regions.push(NvmRegion {
                range: memory.start as u32..memory.start as u32 + memory.size as u32,
                is_boot_memory: memory.startup,
                cores: vec!["main".to_owned()],
            });
        }
    }

    if regions.len() > 1 {
        // Sort by start address
        regions.sort_by_key(|r| r.range.start);

        // Merge contiguous flash regions
        let mut merged = Vec::new();
        let mut cur = regions.first().cloned().unwrap();

        for region in regions.iter().skip(1) {
            if region.range.start == cur.range.end {
                cur.range.end = region.range.end;
            } else {
                merged.push(cur);
                cur = region.clone();
            }
        }

        merged.push(cur);
        regions = merged;
    }

    // Return lowest-addressed region
    regions.first().cloned()
}
