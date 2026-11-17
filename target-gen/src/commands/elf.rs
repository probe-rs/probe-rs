use anyhow::{Context, Result, bail};
use probe_rs_target::{
    ArmCoreAccessOptions, Chip, ChipFamily, Core, CoreAccessOptions, CoreType, MemoryAccess,
    MemoryRange as _, MemoryRegion, NvmRegion, RamRegion, RawFlashAlgorithm,
    TargetDescriptionSource,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write,
    fs::OpenOptions,
    io::Write as _,
    ops::Range,
    path::Path,
    sync::Mutex,
};
use yaml_serde::Value;

use crate::{parser::extract_flash_algo, yaml_update};

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

    let mut algorithm = extract_flash_algo(None, &elf_file, file, true, fixed_load_address)
        .context("Failed to extract flash algorithm from ELF file")?;

    if let Some(name) = &name {
        algorithm.name = name.clone();
    }

    if update {
        // Update an existing target file

        let target_description_file = output.unwrap(); // Argument is checked by structopt, so we know its present.

        let original = std::fs::read_to_string(target_description_file).with_context(|| {
            format!(
                "Unable to open target specification '{}'",
                target_description_file.display()
            )
        })?;

        let mut family: ChipFamily = yaml_serde::from_str(&original)
            .context("Failed to deserialize target description file")?;
        let original_family = family.clone();

        let Some(algorithm_to_update) = family
            .flash_algorithms
            .iter()
            .position(|old_algorithm| old_algorithm.name == algorithm.name)
        else {
            bail!(
                "Unable to update flash algorithm in target description file '{}'. Did not find an existing algorithm with name '{}'",
                target_description_file.display(),
                algorithm.name
            )
        };

        let current = &family.flash_algorithms[algorithm_to_update];
        // Re-extract the algorithm, keeping existing values in the target definition
        let mut algorithm = extract_flash_algo(
            Some(current.clone()),
            &elf_file,
            file,
            current.default,
            fixed_load_address,
        )
        .context("Failed to re-extract flash algorithm from ELF file")?;
        if let Some(name) = name {
            algorithm.name = name;
        }

        // if a load address was specified, use it in the replacement
        if let Some(load_addr) = current.load_address {
            algorithm.load_address = Some(load_addr);
        }
        // core access cannot be determined, use the current value
        algorithm.cores.clone_from(&current.cores);
        algorithm.description.clone_from(&current.description);
        algorithm.rtt_poll_interval = current.rtt_poll_interval;

        family.flash_algorithms[algorithm_to_update] = algorithm;

        let output_yaml = match yaml_update::apply_change(&original, &original_family, &family) {
            Ok(edited) => edited,
            Err(error) => {
                log::warn!("Rewriting the whole target description, comments are lost: {error:#}");
                serialize_to_yaml_string(&family)
                    .context("Failed to serialize updated target description")?
            }
        };
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
                        ap: probe_rs_target::ApAddress::V1(0),
                        targetsel: None,
                        debug_base: None,
                        cti_base: None,
                        jtag_tap: None,
                    }),
                }],
                part: None,
                svd: None,
                documentation: HashMap::new(),
                package_variants: vec![],
                name: "<chip name>".to_owned(),
                memory_map: vec![
                    MemoryRegion::Nvm(NvmRegion {
                        access: None,
                        range: 0..0x2000,
                        memory_ports: vec![],
                        cores: vec!["main".to_owned()],
                        name: None,
                        is_alias: false,
                    }),
                    MemoryRegion::Ram(RamRegion {
                        range: 0x1_0000..0x2_0000,
                        cores: vec!["main".to_owned()],
                        memory_ports: vec![],
                        name: None,
                        is_alias: false,
                        access: Some(MemoryAccess {
                            boot: true,
                            ..Default::default()
                        }),
                    }),
                ],
                flash_algorithms: vec![algorithm_name],
                rtt_scan_ranges: None,
                jtag: None,
                default_binary_format: None,
                memory_ports: vec![],
                skip_reset_on_ram_boot: false,
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

pub(crate) fn compact(family: &ChipFamily) -> ChipFamily {
    let mut out = family.clone();

    sort_memory_regions(&mut out);
    clear_default_memory_access(&mut out);
    clear_default_stack_overflow_check(&mut out);
    compact_flash_algos(&mut out);

    out
}

fn clear_default_stack_overflow_check(out: &mut ChipFamily) {
    for algo in &mut out.flash_algorithms {
        if algo.stack_overflow_check == Some(true) {
            algo.stack_overflow_check = None;
        }
    }
}

fn clear_default_memory_access(out: &mut ChipFamily) {
    for variant in &mut out.variants {
        for region in &mut variant.memory_map {
            let access = match region {
                MemoryRegion::Ram(region) => &mut region.access,
                MemoryRegion::Generic(region) => &mut region.access,
                MemoryRegion::Nvm(region) => &mut region.access,
            };
            if *access == Some(MemoryAccess::default()) {
                *access = None;
            }
        }
    }
}

fn sort_memory_regions(out: &mut ChipFamily) {
    for variant in &mut out.variants {
        variant
            .memory_map
            .sort_by_key(|region| region.address_range().start);
    }
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
    let mut algos_iter = algos.iter();
    while let Some(algo) = algos_iter.next() {
        if renames.contains_key(&algo.name) {
            continue;
        }

        // Collect renames because the new name may change during looping
        let mut renamed = vec![algo.name.clone()];

        // Find the algo with the widest address range and replace all others with it.
        let algo_template = comparable_algo(algo);
        let mut widest_algo = algo.clone();
        for algo_b in algos_iter.clone() {
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
        if widest_algo.name != algo.name {
            // Keep the original algo, too. We will remove these if no uses remain.
            out.flash_algorithms.push(algo.clone());
        }
        out.flash_algorithms.push(widest_algo);
    }

    fn memory_range_of_algo(algo_name: &str, algos: &[RawFlashAlgorithm]) -> Option<Range<u64>> {
        algos
            .iter()
            .find(|a| a.name == algo_name)
            .map(|a| &a.flash_properties.address_range)
            .cloned()
    }

    // Now walk through the target variants' flash algo map and apply the renames
    for variant in &mut out.variants {
        for algo_name in &mut variant.flash_algorithms {
            let Some(replacement_name) = renames.get(algo_name) else {
                continue;
            };

            // Only algos that have memory regions in the memory map are subject to renaming.
            let memory_range = memory_range_of_algo(algo_name, &algos)
                .unwrap_or_else(|| panic!("Flash algorithm {algo_name} not found."));

            if !variant
                .memory_map
                .iter()
                .any(|region| region.address_range().intersects_range(&memory_range))
            {
                // This flash algo has no memory region, so we conjure up one ourselves. We can
                // only deduplicate these algos if the replacement has the same size.
                let replacement_memory_range = memory_range_of_algo(replacement_name, &algos)
                    .unwrap_or_else(|| panic!("Flash algorithm {replacement_name} not found."));

                if replacement_memory_range != memory_range {
                    continue;
                }
            }

            // Apply rename.
            algo_name.clone_from(replacement_name);
        }
    }

    // Remove flash algos with no uses
    out.flash_algorithms.retain(|algo| {
        out.variants
            .iter()
            .any(|variant| variant.flash_algorithms.contains(&algo.name))
    });
}

/// Some optimizations to improve the readability of the `yaml_serde` output:
/// - Fields that hold their default value (`null`, `[]`, `{}`, `false`, and a few others) are
///   omitted, if the output loads back the same without them.
/// - `yaml_serde` serializes hex formatted integers as single quoted strings, e.g. '0x1234' ... we need to remove the single quotes so that it round-trips properly.
pub fn serialize_to_yaml_string(family: &ChipFamily) -> Result<String> {
    serialize_uncompacted(&compact(family))
}

/// Serializes `family` like [`serialize_to_yaml_string`], but keeps the order of its items.
pub(crate) fn serialize_uncompacted(family: &ChipFamily) -> Result<String> {
    let mut value = yaml_serde::to_value(family)?;
    omit_defaults(&mut value);
    to_yaml_string(&value)
}

fn to_yaml_string(value: &Value) -> Result<String> {
    let raw_yaml_string = yaml_serde::to_string(value)?;

    let mut yaml_string = String::with_capacity(raw_yaml_string.len());
    for reader_line in raw_yaml_string.lines() {
        // Write `!Tag {}` as `!Tag`.
        let reader_line = match reader_line.strip_suffix(" {}") {
            Some(tagged)
                if tagged
                    .rsplit(' ')
                    .next()
                    .is_some_and(|t| t.starts_with('!')) =>
            {
                tagged
            }
            _ => reader_line,
        };

        let mut reader_line = Cow::Borrowed(reader_line);
        if (reader_line.contains("'0x") || reader_line.contains("'0X"))
            && (reader_line.ends_with('\'') || reader_line.contains("':"))
        {
            // Remove the single quotes
            reader_line = reader_line.replace('\'', "").into();
        }

        yaml_string.write_str(&reader_line)?;
        yaml_string.push('\n');
    }

    Ok(yaml_string)
}

/// Removes the fields of the serialized family `value` that look like a default value, and that
/// the family loads back the same without.
///
/// The fields are tried in groups of the same path and value.
fn omit_defaults(value: &mut Value) {
    // Whether a group can be omitted depends only on the types of the family, so the verdict
    // holds for every family.
    static OMITTABLE: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());

    let expected = value.clone();
    let loads_back = |value: &Value| {
        to_yaml_string(value)
            .ok()
            .and_then(|yaml| yaml_serde::from_str::<ChipFamily>(&yaml).ok())
            .and_then(|family| yaml_serde::to_value(family).ok())
            .is_some_and(|loaded| loaded == expected)
    };

    loop {
        let mut omit = BTreeSet::new();
        let mut unknown = vec![];
        {
            let verdicts = OMITTABLE.lock().unwrap();
            remove_default_like(value, "", &mut |group| {
                match verdicts.get(&group) {
                    Some(true) => _ = omit.insert(group),
                    Some(false) => {}
                    None if unknown.contains(&group) => {}
                    None => unknown.push(group),
                }
                false
            });
        }
        if omit.is_empty() && unknown.is_empty() {
            return;
        }

        let omittable = omittable_groups(value, &unknown, &loads_back);
        let mut verdicts = OMITTABLE.lock().unwrap();
        for group in unknown {
            let omittable = omittable.contains(&group);
            verdicts.insert(group.clone(), omittable);
            if omittable {
                omit.insert(group);
            }
        }
        drop(verdicts);

        remove_default_like(value, "", &mut |group| omit.contains(&group));
    }
}

/// Returns the `groups` that `value` loads back the same without. Halves the groups until they
/// load back, so that a few groups that must stay cost a few loads.
fn omittable_groups(
    value: &Value,
    groups: &[String],
    loads_back: &impl Fn(&Value) -> bool,
) -> BTreeSet<String> {
    if groups.is_empty() {
        return BTreeSet::new();
    }
    let mut trimmed = value.clone();
    remove_default_like(&mut trimmed, "", &mut |group| groups.contains(&group));
    if loads_back(&trimmed) {
        return groups.iter().cloned().collect();
    }
    if groups.len() == 1 {
        return BTreeSet::new();
    }
    let (first, second) = groups.split_at(groups.len() / 2);
    let mut omittable = omittable_groups(value, first, loads_back);
    omittable.extend(omittable_groups(value, second, loads_back));
    omittable
}

/// Removes the default-like fields whose group `remove` returns `true` for. The group names the
/// path of the field, without sequence indices, and its value.
fn remove_default_like(value: &mut Value, path: &str, remove: &mut impl FnMut(String) -> bool) {
    match value {
        Value::Sequence(items) => {
            for item in items {
                remove_default_like(item, path, remove);
            }
        }
        Value::Mapping(fields) => fields.retain(|key, field| {
            let Some(key) = key.as_str() else {
                return true;
            };
            let path = format!("{path}.{key}");
            remove_default_like(field, &path, remove);
            !default_like(key, field).is_some_and(|value| remove(format!("{path}={value}")))
        }),
        Value::Tagged(tagged) => {
            let path = format!("{path}{}", tagged.tag);
            remove_default_like(&mut tagged.value, &path, remove);
        }
        _ => {}
    }
}

/// Returns `field` as text if it can be the default value of the field `key`.
fn default_like(key: &str, field: &Value) -> Option<String> {
    match field {
        Value::Null => Some("null".to_owned()),
        Value::Sequence(items) if items.is_empty() => Some("[]".to_owned()),
        Value::Mapping(fields) if fields.is_empty() => Some("{}".to_owned()),
        Value::Bool(value) => Some(value.to_string()),
        Value::String(string) if string.is_empty() => Some("''".to_owned()),
        // These fields have a default that is not the default of their type.
        Value::Number(number) if key == "rtt_poll_interval" => Some(number.to_string()),
        Value::String(string) if key == "scan_mode" => Some(string.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod test {
    use probe_rs_target::TargetDescriptionSource;

    use super::*;

    #[test]
    fn test_serialize_to_yaml_string_cuts_off_unnecessary_defaults() {
        let mut chip = Chip::generic_arm("Test Chip", CoreType::Armv8m);

        chip.memory_map.push(MemoryRegion::Ram(RamRegion {
            range: 0x20000000..0x20004000,
            cores: vec!["main".to_owned()],
            memory_ports: vec![],
            name: Some(String::from("SRAM")),
            access: Some(MemoryAccess::default()),
            is_alias: false,
        }));
        chip.memory_map.push(MemoryRegion::Ram(RamRegion {
            range: 0x20004000..0x20008000,
            cores: vec!["main".to_owned()],
            memory_ports: vec![],
            name: Some(String::from("CCMRAM")),
            is_alias: false,
            access: Some(MemoryAccess {
                boot: false,
                read: true,
                write: true,
                execute: false,
            }),
        }));
        chip.memory_map.push(MemoryRegion::Nvm(NvmRegion {
            range: 0x8000000..0x8010000,
            cores: vec!["main".to_owned()],
            memory_ports: vec![],
            name: Some(String::from("Flash")),
            access: None,
            is_alias: false,
        }));

        let family = ChipFamily {
            name: "Test Family".to_owned(),
            manufacturer: None,
            generated_from_pack: false,
            chip_detection: vec![],
            pack_file_release: None,
            variants: vec![chip],
            flash_algorithms: vec![],
            source: TargetDescriptionSource::BuiltIn,
        };
        let yaml_string = serialize_to_yaml_string(&family).unwrap();
        insta::assert_snapshot!("serialization_cleanup", yaml_string);
    }

    #[test]
    fn serialize_to_yaml_string_round_trips_builtin_targets() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut failures = vec![];
        for dir in ["probe-rs/targets", "probe-rs-espressif/targets"] {
            for entry in std::fs::read_dir(workspace.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let yaml = std::fs::read_to_string(&path).unwrap();
                let family: ChipFamily = yaml_serde::from_str(&yaml).unwrap();

                let serialized = serialize_to_yaml_string(&family).unwrap();
                let round_tripped = match yaml_serde::from_str::<ChipFamily>(&serialized) {
                    Ok(round_tripped) => round_tripped,
                    Err(error) => {
                        failures.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                };

                let expected = yaml_serde::to_value(compact(&family)).unwrap();
                if yaml_serde::to_value(round_tripped).unwrap() != expected {
                    failures.push(format!("{}: contents changed", path.display()));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn serialize_to_yaml_string_keeps_required_empty_fields() {
        let yaml = "\
name: Test Family
chip_detection:
- !TiIcepick
  icepick_id: 0x1234
  variants: []
- !NordicConfigId
  configid_address: 0x10000130
  hwid: {}
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  - !Ram
    range:
      start: 0x20000000
      end: 0x20004000
    cores: []
";
        let family: ChipFamily = yaml_serde::from_str(yaml).unwrap();

        let serialized = serialize_to_yaml_string(&family).unwrap();
        let round_tripped: ChipFamily = yaml_serde::from_str(&serialized).unwrap();
        assert_eq!(
            yaml_serde::to_value(round_tripped).unwrap(),
            yaml_serde::to_value(compact(&family)).unwrap()
        );
    }
}
