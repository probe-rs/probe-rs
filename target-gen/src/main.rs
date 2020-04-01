pub mod algorithm_binary;
pub mod chip;
pub mod chip_family;
pub mod error;
pub mod flash_device;
pub mod parser;
pub mod raw_flash_algorithm;

use crate::error::Error;
use crate::raw_flash_algorithm::RawFlashAlgorithm;
use chip::Chip;
use chip_family::ChipFamily;
use cmsis_pack::pdsc::Core;
use cmsis_pack::pdsc::Device;
use cmsis_pack::pdsc::Package;
use cmsis_pack::pdsc::Processors;
use cmsis_pack::utils::FromElem;
use pretty_env_logger;
use probe_rs::config::{FlashRegion, MemoryRegion, RamRegion};
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use structopt::StructOpt;

use log;

#[derive(StructOpt)]
struct Options {
    #[structopt(name = "INPUT_DIR", parse(from_os_str))]
    input_dir: PathBuf,
    #[structopt(name = "OUTPUT_DIR", parse(from_os_str))]
    output_dir: PathBuf,
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

fn main() {
    pretty_env_logger::init();

    let options = Options::from_args();
    // The directory in which to look for the .pdsc file.
    let in_dir = options.input_dir;
    let out_dir = options.output_dir;

    if !in_dir.exists() {
        panic!("No such file or directory {:?}", in_dir);
    }
    else if !out_dir.exists() {
        panic!("No such file or directory {:?}", out_dir);
    }

    let mut families = Vec::<ChipFamily>::new();
    // Look for the .pdsc file in the given dir and it's child directories.
    let generation_result = visit_dirs(Path::new(&in_dir), &mut |pdsc, mut archive| {
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
                            archive
                                .by_name(&flash_algorithm.file_name.as_path().to_string_lossy())
                                .unwrap(),
                            flash_algorithm.file_name.as_path(),
                            flash_algorithm.default,
                        )
                    } else {
                        crate::parser::extract_flash_algo(
                            std::fs::File::open(in_dir.join(&flash_algorithm.file_name).as_path())
                                .unwrap(),
                            flash_algorithm.file_name.as_path(),
                            flash_algorithm.default,
                        )
                    }?;

                    Ok(algo)
                })
                .filter_map(|flash_algorithm: Result<RawFlashAlgorithm, Error>| {
                    match flash_algorithm {
                        Ok(flash_algorithm) => Some(flash_algorithm),
                        Err(error) => {
                            log::warn!("Failed to parse flash algorithm.");
                            log::warn!("Reason: {:?}", error);
                            None
                        }
                    }
                })
                .collect::<Vec<_>>();

            // Extract the flash info from the .pdsc file.
            let mut flash = None;
            for memory in device.memories.0.values() {
                if memory.default
                    && memory.access.read
                    && memory.access.execute
                    && !memory.access.write
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
                        return Err(Error::Unsupported(format!(
                            "Core '{:?}' is not yet supported for target generation.",
                            c
                        )));
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
    });

    if let Err(e) = generation_result {
        eprintln!("Failed to generate target configuration: {}", e);
        std::process::exit(-1);
    }

    for family in &families {
        let path = out_dir.join(family.name.clone() + ".yaml");
        let file = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("Opening {:?} failed: {}", path, e));
        serde_yaml::to_writer(file, &family).unwrap();
    }
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs<T>(
    path: &Path,
    cb: &mut dyn FnMut(Package, Option<&mut zip::ZipArchive<File>>) -> Result<T, Error>,
) -> Result<(), Error> {
    // If we get a dir, look for all .pdsc files.
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else if let Some(extension) = path.as_path().extension() {
                if extension == "pdsc" {
                    log::info!("Found .pdsc file: {}", path.display());
                    cb(Package::from_path(entry.path().as_path()).unwrap(), None)?;
                }
            }
        }
    } else if let Some(extension) = path.extension() {
        if extension == "pack" {
            log::info!("Found .pack file: {}", path.display());
            // If we get a file, try to unpack it.
            let file = fs::File::open(&path).unwrap();

            match zip::ZipArchive::new(file) {
                Ok(mut archive) => {
                    let pdsc =
                        find_pdsc_in_archive(&mut archive).map_or_else(String::new, |mut pdsc| {
                            let mut pdsc_string = String::new();
                            use std::io::Read;
                            pdsc.read_to_string(&mut pdsc_string).unwrap();
                            pdsc_string
                        });
                    cb(Package::from_string(&pdsc).unwrap(), Some(&mut archive))?;
                }
                Err(e) => {
                    log::error!("Zip file could not be read. Reason:");
                    log::error!("{:?}", e);
                    std::process::exit(1);
                }
            };
        }
    }
    Ok(())
}

/// Extracts the pdsc out of a ZIP archive.
fn find_pdsc_in_archive(archive: &mut zip::ZipArchive<File>) -> Option<zip::read::ZipFile> {
    let mut index = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i).unwrap();
        let outpath = file.sanitized_name();

        if let Some(extension) = outpath.as_path().extension() {
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
