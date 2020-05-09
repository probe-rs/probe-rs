pub mod algorithm_binary;
pub mod flash_device;
pub mod parser;

use anyhow::{anyhow, bail, ensure, Context, Result};
use cmsis_pack::pdsc::{Core, Device, Package, Processors};
use cmsis_pack::utils::FromElem;
use log;
use pretty_env_logger;
use probe_rs::config::{Chip, ChipFamily, FlashRegion, MemoryRegion, RamRegion, RawFlashAlgorithm};
use structopt::StructOpt;

use parser::extract_flash_algo;
use std::{
    borrow::Cow,
    fs::{self, create_dir, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(StructOpt)]
enum TargetGen {
    /// Generate target description from ARM CMSIS-Packs
    Pack {
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
    },
    /// Extract a flash algorithm from an ELF file
    Extract {
        /// ELF file containing a flash algorithm
        #[structopt(parse(from_os_str))]
        elf: PathBuf,
        /// Name of the extracted flash algorithm
        #[structopt(long = "name", short = "n")]
        name: Option<String>,
        /// Update an existing flash algorithm
        #[structopt(long = "update", short = "u", requires = "output")]
        update: bool,
        /// Output file, if provided, the generated target description will be written to this file.
        #[structopt(parse(from_os_str))]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let options = TargetGen::from_args();

    match options {
        TargetGen::Pack { input, output_dir } => cmd_pack(&input, &output_dir)?,
        TargetGen::Extract {
            elf,
            output,
            update,
            name,
        } => cmd_extract(elf, output, update, name)?,
    }

    Ok(())
}

/// Prepare a target config based on an ELF file containing a flash algorithm.
fn cmd_extract(
    file: PathBuf,
    output: Option<PathBuf>,
    update: bool,
    name: Option<String>,
) -> Result<()> {
    let elf_file = File::open(&file)?;

    let mut algorithm = extract_flash_algo(elf_file, &file, true)?;

    if let Some(name) = name {
        algorithm.name = Cow::Owned(name);
    }

    if update {
        // Update an existing target file

        let target_description_file = output.unwrap(); // Argument is checked by structopt, so we now its present.

        let target_description = File::open(&target_description_file).context(format!(
            "Unable to open target specification '{}'",
            target_description_file.display()
        ))?;

        let mut family = probe_rs::config::ChipFamily::from_yaml_reader(&target_description)?;

        // Close target description file, we want to overwrite it later
        drop(target_description);

        let algorithm_to_update = family
            .flash_algorithms
            .iter()
            .position(|old_algorithm| old_algorithm.name == algorithm.name);

        match algorithm_to_update {
            None => bail!("Unable to update flash algorithm in target description file '{}'. Did not find an existing algorithm with name '{}'", target_description_file.display(), &algorithm.name),
            Some(index) => family.flash_algorithms.to_mut()[index] = algorithm,
        }

        let target_description = File::create(&target_description_file)?;

        serde_yaml::to_writer(&target_description, &family)?;
    } else {
        // Create a complete target specification, with place holder values
        let algorithm_name = algorithm.name.clone();

        let chip_family = ChipFamily {
            name: Cow::Borrowed("<family name>"),
            manufacturer: None,
            variants: Cow::Owned(vec![Chip {
                part: None,
                name: Cow::Borrowed("<chip name>"),
                memory_map: Cow::Borrowed(&[
                    MemoryRegion::Flash(FlashRegion {
                        is_boot_memory: false,
                        range: 0..0x2000,
                    }),
                    MemoryRegion::Ram(RamRegion {
                        is_boot_memory: true,
                        range: 0x1_0000..0x2_0000,
                    }),
                ]),
                flash_algorithms: Cow::Owned(vec![algorithm_name]),
            }]),
            flash_algorithms: Cow::Owned(vec![algorithm]),
            core: Cow::Borrowed("<mcu core>"),
        };

        let serialized = serde_yaml::to_string(&chip_family)?;

        match output {
            Some(output) => {
                // Ensure we don't overwrite an existing file
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)
                    .context(format!(
                        "Failed to create target file '{}'.",
                        output.display()
                    ))?;

                file.write_all(serialized.as_bytes())?;
            }
            None => println!("{}", serialized),
        }
    }

    Ok(())
}

/// Handle the pack subcommand. `input` is either the path
/// to a CMSIS-Pack file, or a directory containing at least one .pdsc file.
///
/// Generated target description will be placed in `out_dir`.
fn cmd_pack(input: &Path, out_dir: &Path) -> Result<()> {
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
        let path = out_dir.join(family.name.clone().into_owned() + ".yaml");
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
            families.push(
                /*
                    ChipFamily::new(
                    device.family,
                    HashMap::new(),
                    core.to_owned(),
                )
                */
                ChipFamily {
                    name: device.family.into(),
                    manufacturer: None,
                    variants: Cow::Owned(Vec::new()),
                    core: core.into(),
                    flash_algorithms: Cow::Borrowed(&[]),
                },
            );
            // This unwrap is always safe as we insert at least one item previously.
            families.last_mut().unwrap()
        };

        let flash_algorithm_names: Vec<_> = variant_flash_algorithms
            .iter()
            .map(|fa| fa.name.clone().to_lowercase())
            .collect();

        for fa in variant_flash_algorithms {
            family.flash_algorithms.to_mut().push(fa);
        }

        let mut memory_map: Vec<MemoryRegion> = Vec::new();
        if let Some(mem) = ram {
            memory_map.push(MemoryRegion::Ram(mem));
        }
        if let Some(mem) = flash {
            memory_map.push(MemoryRegion::Flash(mem));
        }

        family.variants.to_mut().push(Chip {
            name: Cow::Owned(device_name),
            part: None,
            memory_map: Cow::Owned(memory_map),
            flash_algorithms: Cow::Owned(
                flash_algorithm_names.into_iter().map(Cow::Owned).collect(),
            ),
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
