pub mod algorithm_binary;
pub mod chip;
pub mod chip_family;
pub mod flash_device;
pub mod parser;
pub mod raw_flash_algorithm;

use crate::chip::Chip;
use crate::chip_family::ChipFamily;
use crate::raw_flash_algorithm::RawFlashAlgorithm;

use anyhow::{anyhow, bail, ensure, Context, Result};
use cmsis_pack::pdsc::{Core, Device, Package, Processors};
use cmsis_pack::utils::FromElem;
use log;
use pretty_env_logger;
use probe_rs::config::{FlashRegion, MemoryRegion, RamRegion};
use structopt::StructOpt;

use fs::create_dir;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(StructOpt)]
struct Options {
    #[structopt(
        name = "INPUT",
        parse(from_os_str),
        help = "A Pack file or the unziped Pack directory."
    )]
    input: PathBuf,
    #[structopt(
        name = "OUTPUT",
        parse(from_os_str),
        help = "An output directory where all the generated .yaml files are put in."
    )]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let options = Options::from_args();

    // The directory in which to look for the .pdsc file.
    let input = options.input;
    let out_dir = options.output_dir;

    ensure!(
        input.exists(),
        "No such file or directory: {}",
        input.display()
    );

    if !out_dir.exists() {
        create_dir(&out_dir).context(format!(
            "Failed to create output directory '{}'.",
            out_dir.display()
        ))?;
    }

    let mut families = Vec::<ChipFamily>::new();

    if input.is_file() {
        visit_file(&input, &mut families)
            .context(format!("Failed to process file {}.", input.display()))?;
    } else {
        // Look for the .pdsc file in the given dir and it's child directories.
        visit_dirs(&input, &mut families).context("Failed to generate target configuration.")?;

        // Check that we found at least a single .pdsc file
        ensure!(
            !families.is_empty(),
            "Unable to find any .pdsc files in the provided input directory."
        );
    }

    let mut generated_files = Vec::with_capacity(families.len());

    for family in &families {
        let path = out_dir.join(family.name.clone() + ".yaml");
        let file = std::fs::File::create(&path)
            .context(format!("Failed to create file '{}'.", path.display()))?;
        serde_yaml::to_writer(file, &family)?;

        generated_files.push(path);
    }

    println!("Generated {} target definition(s):", generated_files.len());

    for file in generated_files {
        println!("\t{}", file.display());
    }

    Ok(())
}

fn handle_package(
    pdsc: Package,
    mut archive: Option<&mut zip::ZipArchive<File>>,
    input: &Path,
    families: &mut Vec<ChipFamily>,
) -> Result<()> {
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
                let algo = if let Some(ref mut archive) = archive {
                    crate::parser::extract_flash_algo(
                        archive.by_name(&flash_algorithm.file_name.as_path().to_string_lossy())?,
                        &flash_algorithm.file_name,
                        flash_algorithm.default,
                    )
                } else {
                    crate::parser::extract_flash_algo(
                        std::fs::File::open(input.join(&flash_algorithm.file_name))?,
                        &flash_algorithm.file_name,
                        flash_algorithm.default,
                    )
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
        let mut flash = None;
        for memory in device.memories.0.values() {
            if memory.default && memory.access.read && memory.access.execute && !memory.access.write
            {
                flash = Some(FlashRegion {
                    range: memory.start as u32..memory.start as u32 + memory.size as u32,
                    is_boot_memory: memory.startup,
                });
                break;
            }
        }

        // Get the core type.
        let core = if let Processors::Symmetric(processor) = &device.processor {
            match &processor.core {
                Core::CortexM0 => "M0",
                Core::CortexM0Plus => "M0",
                Core::CortexM4 => "M4",
                Core::CortexM3 => "M3",
                Core::CortexM33 => "M33",
                Core::CortexM7 => "M7",
                c => {
                    bail!("Core '{:?}' is not yet supported for target generation.", c);
                }
            }
        } else {
            log::warn!("Asymmetric cores are not supported yet.");
            ""
        };

        // Check if this device family is already known.
        let mut potential_family = families
            .iter_mut()
            .find(|family| family.name == device.family);

        let family = if let Some(ref mut family) = potential_family {
            family
        } else {
            families.push(ChipFamily::new(
                device.family,
                HashMap::new(),
                core.to_owned(),
            ));
            // This unwrap is always safe as we insert at least one item previously.
            families.last_mut().unwrap()
        };

        let flash_algorithm_names: Vec<_> = variant_flash_algorithms
            .iter()
            .map(|fa| fa.name.clone().to_lowercase())
            .collect();

        for fa in variant_flash_algorithms {
            family.flash_algorithms.insert(fa.name.clone(), fa);
        }

        let mut memory_map: Vec<MemoryRegion> = Vec::new();
        if let Some(mem) = ram {
            memory_map.push(MemoryRegion::Ram(mem));
        }
        if let Some(mem) = flash {
            memory_map.push(MemoryRegion::Flash(mem));
        }

        family.variants.push(Chip {
            name: device_name,
            memory_map,
            flash_algorithms: flash_algorithm_names,
        });
    }

    Ok(())
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    // If we get a dir, look for all .pdsc files.
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            visit_dirs(&entry_path, families)?;
        } else if let Some(extension) = entry_path.extension() {
            if extension == "pdsc" {
                log::info!("Found .pdsc file: {}", path.display());

                handle_package(
                    Package::from_path(&entry.path()).map_err(|e| e.compat())?,
                    None,
                    path,
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

fn visit_file(path: &Path, families: &mut Vec<ChipFamily>) -> Result<()> {
    log::info!("Trying to open pack file: {}.", path.display());
    // If we get a file, try to unpack it.
    let file = fs::File::open(&path)?;

    let mut archive = zip::ZipArchive::new(file)?;

    let mut pdsc_file = find_pdsc_in_archive(&mut archive)
        .ok_or_else(|| anyhow!("Failed to find .pdsc file in archive {}", path.display()))?;

    let mut pdsc = String::new();
    pdsc_file.read_to_string(&mut pdsc)?;

    let package = Package::from_string(&pdsc).map_err(|e| {
        anyhow!(
            "Failed to parse pdsc file '{}' in CMSIS Pack {}: {}",
            pdsc_file.sanitized_name().display(),
            path.display(),
            e
        )
    })?;

    drop(pdsc_file);

    handle_package(package, Some(&mut archive), path, families)
}

/// Extracts the pdsc out of a ZIP archive.
fn find_pdsc_in_archive(archive: &mut zip::ZipArchive<File>) -> Option<zip::read::ZipFile> {
    let mut index = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i).unwrap();
        let outpath = file.sanitized_name();

        if let Some(extension) = outpath.extension() {
            if extension == "pdsc" {
                index = Some(i);
                break;
            }
        }
    }
    if let Some(index) = index {
        Some(archive.by_index(index).unwrap())
    } else {
        None
    }
}

fn get_ram(device: &Device) -> Option<RamRegion> {
    for memory in device.memories.0.values() {
        if memory.default && memory.access.read && memory.access.write {
            return Some(RamRegion {
                range: memory.start as u32..memory.start as u32 + memory.size as u32,
                is_boot_memory: memory.startup,
            });
        }
    }

    None
}
