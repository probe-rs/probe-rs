use anyhow::{bail, Context, Result};
use probe_rs_target::{
    ArmCoreAccessOptions, Chip, ChipFamily, Core, CoreAccessOptions, CoreType, MemoryRegion,
    NvmRegion, RamRegion, RawFlashAlgorithm, TargetDescriptionSource,
};
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Write,
    fs::{File, OpenOptions},
    io::Write as _,
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
    let elf_file = std::fs::read(file)
        .with_context(|| format!("Failed to open ELF file {}", file.display()))?;

    let mut algorithm = extract_flash_algo(&elf_file, file, true, fixed_load_address)?;

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
                algorithm.cores.clone_from(&current.cores);
                algorithm.description.clone_from(&current.description);

                family.flash_algorithms[index] = algorithm
            },
        }

        let output_yaml = serialize_to_yaml_string(&family)?;
        std::fs::write(target_description_file, output_yaml)?;
    } else {
        // Create a complete target specification, with place holder values
        let algorithm_name = algorithm.name.clone();
        algorithm.cores = vec!["main".to_owned()];

        let chip_family = ChipFamily {
            name: "<family name>".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            chip_detection: vec![],
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
                svd: None,
                documentation: HashMap::new(),
                name: "<chip name>".to_owned(),
                memory_map: vec![
                    MemoryRegion::Nvm(NvmRegion {
                        is_boot_memory: false,
                        range: 0..0x2000,
                        cores: vec!["main".to_owned()],
                        name: None,
                        is_alias: false,
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
                jtag: None,
                default_binary_format: None,
            }],
            flash_algorithms: vec![algorithm],
            source: TargetDescriptionSource::BuiltIn,
        };

        let output_yaml = serialize_to_yaml_string(&chip_family)?;
        match output {
            Some(output) => {
                // Ensure we don't overwrite an existing file
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output)
                    .context(format!(
                        "Failed to create target file '{}'.",
                        output.display()
                    ))?;

                file.write_all(output_yaml.as_bytes())?;
            }
            None => println!("{output_yaml}"),
        }
    }

    Ok(())
}

fn compact(family: &ChipFamily) -> ChipFamily {
    let mut out = family.clone();

    compact_flash_algos(&mut out);

    out
}

fn compact_flash_algos(out: &mut ChipFamily) {
    fn comparable_algo(algo: &RawFlashAlgorithm) -> RawFlashAlgorithm {
        let mut algo = algo.clone();
        algo.flash_properties.address_range.end = 0;
        algo.description = String::new();
        algo.name = String::new();
        algo
    }

    let mut renames = std::collections::HashMap::<String, String>::new();

    let algos = std::mem::take(&mut out.flash_algorithms);
    let mut algos = algos.iter();
    while let Some(algo) = algos.next() {
        if renames.contains_key(&algo.name) {
            continue;
        }

        // Collect renames because the new name may change during looping
        let mut renamed = vec![algo.name.clone()];

        // Find the algo with the widest address range and replace all others with it.
        let algo_template = comparable_algo(algo);
        let mut widest_algo = algo.clone();
        for algo_b in algos.clone() {
            if renames.contains_key(&algo_b.name) {
                continue;
            }

            if algo_template == comparable_algo(algo_b) {
                renamed.push(algo_b.name.clone());

                if algo_b.flash_properties.address_range.end
                    > widest_algo.flash_properties.address_range.end
                {
                    widest_algo = algo_b.clone();
                }
            }
        }

        for renamed in renamed {
            renames.insert(renamed, widest_algo.name.clone());
        }
        out.flash_algorithms.push(widest_algo);
    }

    // Now walk through the target variants' flash algo map and apply the renames
    for variant in &mut out.variants {
        for flash_algo in &mut variant.flash_algorithms {
            if let Some(new_name) = renames.get(flash_algo) {
                flash_algo.clone_from(new_name);
            }
        }
    }
}

/// Some optimizations to improve the readability of the `serde_yaml` output:
/// - If `Option<T>` is `None`, it is serialized as `null` ... we want to omit it.
/// - If `Vec<T>` is empty, it is serialized as `[]` ... we want to omit it.
/// - `serde_yaml` serializes hex formatted integers as single quoted strings, e.g. '0x1234' ... we need to remove the single quotes so that it round-trips properly.
pub fn serialize_to_yaml_string(family: &ChipFamily) -> Result<String> {
    let family = compact(family);
    let raw_yaml_string = serde_yaml::to_string(&family)?;

    let mut yaml_string = String::with_capacity(raw_yaml_string.len());
    for reader_line in raw_yaml_string.lines() {
        if reader_line.ends_with(": null")
            || reader_line.ends_with(": []")
            || reader_line.ends_with(": {}")
            || reader_line.ends_with(": false")
        {
            // Some fields have default-looking, but significant values that we want to keep.
            let exceptions = ["rtt_scan_ranges: []"];
            if !exceptions.contains(&reader_line.trim()) {
                // Skip the line
                continue;
            }
        }

        let mut reader_line = Cow::Borrowed(reader_line);
        if (reader_line.contains("'0x") || reader_line.contains("'0X"))
            && reader_line.ends_with('\'')
        {
            // Remove the single quotes
            reader_line = reader_line.replace('\'', "").into();
        }

        yaml_string.write_str(&reader_line)?;
        yaml_string.push('\n');
    }

    Ok(yaml_string)
}

#[cfg(test)]
mod test {
    use probe_rs_target::TargetDescriptionSource;

    use super::*;

    #[test]
    fn test_serialize_to_yaml_string_cuts_off_unnecessary_defaults() {
        let family = ChipFamily {
            name: "Test Family".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            chip_detection: vec![],
            pack_file_release: None,
            variants: vec![Chip::generic_arm("Test Chip", CoreType::Armv8m)],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::BuiltIn,
        };
        let yaml_string = serialize_to_yaml_string(&family).unwrap();
        let expectation = "name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: 0
      psel: 0x0
  default_binary_format: raw
";
        assert_eq!(yaml_string, expectation);
    }
}
