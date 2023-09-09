use anyhow::{bail, Context, Result};
use probe_rs::CoreType;
use probe_rs_target::{
    ArmCoreAccessOptions, Chip, ChipFamily, Core, CoreAccessOptions, MemoryRegion, NvmRegion,
    RamRegion, TargetDescriptionSource::BuiltIn,
};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, Write},
    path::Path,
};

use crate::parser::extract_flash_algo;

/// Prepare a target config based on an ELF file containing a flash algorithm.
pub fn cmd_elf(
    file: &Path,
    fixed_load_address: bool,
    output: Option<&Path>,
    update: bool,
    name: Option<String>,
) -> Result<()> {
    let elf_file = File::open(file)?;

    let mut algorithm = extract_flash_algo(elf_file, file, true, fixed_load_address)?;

    if let Some(name) = name {
        algorithm.name = name;
    }

    if update {
        // Update an existing target file

        let target_description_file = output.unwrap(); // Argument is checked by structopt, so we now its present.

        let target_description = File::open(target_description_file).context(format!(
            "Unable to open target specification '{}'",
            target_description_file.display()
        ))?;

        let mut family: ChipFamily = serde_yaml::from_reader(target_description)?;

        let algorithm_to_update = family
            .flash_algorithms
            .iter()
            .position(|old_algorithm| old_algorithm.name == algorithm.name);

        match algorithm_to_update {
            None => bail!("Unable to update flash algorithm in target description file '{}'. Did not find an existing algorithm with name '{}'", target_description_file.display(), &algorithm.name),
            Some(index) => {
                let current = &family.flash_algorithms[index];

                // if a load address was specified, use it in the replacement
                if let Some(load_addr)  = current.load_address {
                    algorithm.load_address = Some(load_addr);
                    algorithm.data_section_offset = algorithm.data_section_offset.saturating_sub(load_addr);
                }
                // core access cannot be determined, use the current value
                algorithm.cores = current.cores.clone();
                algorithm.description = current.description.clone();

                family.flash_algorithms[index] = algorithm
            },
        }

        let target_description = File::create(target_description_file)?;
        serialize_to_yaml_file(&family, &target_description)?;
    } else {
        // Create a complete target specification, with place holder values
        let algorithm_name = algorithm.name.clone();
        algorithm.cores = vec!["main".to_owned()];

        let chip_family = ChipFamily {
            name: "<family name>".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            pack_file_release: None,
            variants: vec![Chip {
                cores: vec![Core {
                    name: "main".to_owned(),
                    core_type: CoreType::Armv6m,
                    core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions {
                        ap: 0,
                        psel: 0,
                        debug_base: None,
                        cti_base: None,
                    }),
                }],
                part: None,
                name: "<chip name>".to_owned(),
                memory_map: vec![
                    MemoryRegion::Nvm(NvmRegion {
                        is_boot_memory: false,
                        range: 0..0x2000,
                        cores: vec!["main".to_owned()],
                        name: None,
                    }),
                    MemoryRegion::Ram(RamRegion {
                        is_boot_memory: true,
                        range: 0x1_0000..0x2_0000,
                        cores: vec!["main".to_owned()],
                        name: None,
                    }),
                ],
                flash_algorithms: vec![algorithm_name],
                rtt_scan_ranges: None,
            }],
            flash_algorithms: vec![algorithm],
            source: BuiltIn,
        };

        match output {
            Some(output) => {
                // Ensure we don't overwrite an existing file
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output)
                    .context(format!(
                        "Failed to create target file '{}'.",
                        output.display()
                    ))?;
                serialize_to_yaml_file(&chip_family, &file)?;
            }
            None => println!("{}", serde_yaml::to_string(&chip_family)?),
        }
    }

    Ok(())
}

/// Some optimizations to improve the readability of the `serde_yaml` output:
/// - If `Option<T>` is `None`, it is serialized as `null` ... we want to omit it.
/// - If `Vec<T>` is empty, it is serialized as `[]` ... we want to omit it.
/// - `serde_yaml` serializes hex formatted integers as single quoted strings, e.g. '0x1234' ... we need to remove the single quotes so that it round-trips properly.
pub fn serialize_to_yaml_file(family: &ChipFamily, file: &File) -> Result<(), anyhow::Error> {
    let yaml_string = serde_yaml::to_string(&family)?;
    let mut reader = std::io::BufReader::new(yaml_string.as_bytes());
    let mut reader_line = String::new();
    let mut writer = std::io::BufWriter::new(file);
    while reader.read_line(&mut reader_line)? > 0 {
        if reader_line.ends_with(": null\n")
            || reader_line.ends_with(": []\n")
            || reader_line.ends_with(": false\n")
        {
            // Skip the line
        } else if (reader_line.contains("'0x") || reader_line.contains("'0X"))
            && reader_line.ends_with("'\n")
        {
            // Remove the single quotes
            reader_line = reader_line.replace('\'', "");
            writer.write_all(reader_line.as_bytes())?;
        } else {
            writer.write_all(reader_line.as_bytes())?;
        }
        reader_line.clear();
    }
    Ok(())
}
