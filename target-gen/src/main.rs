pub mod algorithm_binary;
pub mod flash_device;
pub mod parser;
pub mod chip;

use probe_rs::config::memory::{RamRegion, FlashRegion, MemoryRegion};
use std::fs::{self, DirEntry};
use std::io;
use std::path::Path;
use cmsis_pack::utils::FromElem;
use cmsis_pack::pdsc::Package;
use cmsis_pack::pdsc::Processors;
use cmsis_pack::pdsc::Core;
use chip::Chip;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    // The directory in which to look for the .pdsc file.
    let in_dir = &std::path::Path::new(&args[1]);
    let out_dir = &std::path::Path::new(&args[2]);

    let mut chips = Vec::<Chip>::new();

    // Look for the .pdsc file in the given dir and it's child directories.
    visit_dirs(Path::new(&in_dir), &mut |entry| {
        // Parse the .pdsc file.
        Package::from_path(&entry.path())
            .map(|p| {
                // Forge a definition file for each device in the .pdsc file.
                for (device_name, device) in p.devices.0 {
                    // Extract the RAM info from the .pdsc file.
                    let mut ram = None;
                    for (_name, memory) in &device.memories.0 {
                        if memory.access.read && memory.access.write {
                            ram = Some(RamRegion {
                                range: memory.start as u32..memory.start as u32 + memory.size as u32,
                                is_boot_memory: memory.startup,
                            });
                            break;
                        }
                    }

                    // Extract the flash algorithm, block & sector size and the erased byte value from the ELF binary.
                    let mut page_size = 0;
                    let mut sector_size = 0;
                    let mut erased_byte_value = 0xFF;
                    let flash_algorithms = device.algorithms.iter().map(|flash_algorithm| {
                        let (algo, ps, ss, ebv) = crate::parser::extract_flash_algo(
                            in_dir.join(&flash_algorithm.file_name).as_path(),
                            ram.as_ref().unwrap().clone(),
                            flash_algorithm.default
                        );

                        page_size = ps;
                        sector_size = ss;
                        erased_byte_value = ebv;

                        algo
                    }).collect::<Vec<_>>();

                    // Extract the flash info from the .pdsc file.
                    let mut flash = None;
                    for (_name, memory) in &device.memories.0 {
                        if memory.access.read && memory.access.execute {
                            flash = Some(FlashRegion {
                                range: memory.start as u32..memory.start as u32 + memory.size as u32,
                                is_boot_memory: memory.startup,
                                sector_size,
                                page_size,
                                erased_byte_value,
                            });
                            break;
                        }
                    }

                    let core = if let Processors::Symmetric(processor) = device.processor {
                        match processor.core {
                            Core::CortexM0 => "M0",
                            Core::CortexM0Plus => "M0",
                            Core::CortexM4 => "M4",
                            Core::CortexM3 => "M3",
                            c => {
                                log::warn!("Core {:?} not supported yet.", c);
                                ""
                            },
                        }
                    } else {
                        log::warn!("Asymmetric cores are not supported yet.");
                        ""
                    };

                    let chip = Chip {
                        name: device_name,
                        flash_algorithms,
                        memory_map: vec![
                            MemoryRegion::Ram(ram.unwrap()),
                            MemoryRegion::Flash(flash.unwrap()),
                        ],
                        core: core.to_owned(),
                    };

                    chips.push(chip)
                }
            })
            .unwrap();
    })
    .unwrap();

    for chip in &chips {
        let file = std::fs::File::create(out_dir.join(chip.name.clone() + ".yaml")).unwrap();
        serde_yaml::to_writer(file, &chip).unwrap();
    }

    println!("{:#?}", chips);
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, cb: &mut dyn FnMut(&DirEntry)) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".pdsc")
            {
                cb(&entry);
            }
        }
    }
    Ok(())
}
