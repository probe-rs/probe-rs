//! Applies a change to a target description YAML file without rewriting the parts that did
//! not change, so that comments and formatting there survive.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use probe_rs_target::ChipFamily;
use yaml_edit::{Mapping, Sequence, YamlFile, YamlNode};
use yaml_serde::Value;

use crate::commands::elf::{compact, serialize_to_yaml_string, serialize_uncompacted};

/// Edits `original`, the YAML text of `base`, so that it describes `updated`.
///
/// Fails if the edited text does not describe `updated`.
pub fn apply_change(original: &str, base: &ChipFamily, updated: &ChipFamily) -> Result<String> {
    let base_yaml = serialize_uncompacted(base)?;
    let updated_yaml = serialize_uncompacted(updated)?;
    let edited = apply_yaml_change(original, &base_yaml, &updated_yaml)?;
    check_describes(&edited, updated)?;
    Ok(edited)
}

/// Edits `original`, the YAML text of `family`, towards the output of
/// [`serialize_to_yaml_string`]: it drops default values and sorts the memory regions.
///
/// Values that parse the same, like `0x08000000` and `0x8000000`, keep their spelling.
/// Fails if the edited text does not describe `family` with sorted regions and merged flash
/// algorithms.
pub fn reformat(original: &str, family: &ChipFamily) -> Result<String> {
    let reformatted = serialize_to_yaml_string(family)?;
    let edited = apply_yaml_change(original, original, &reformatted)?;
    check_describes(&edited, &compact(family))?;
    Ok(edited)
}

fn check_describes(edited: &str, family: &ChipFamily) -> Result<()> {
    let edited_family: ChipFamily =
        yaml_serde::from_str(edited).context("The edited file does not load")?;
    if yaml_serde::to_value(edited_family)? != yaml_serde::to_value(family)? {
        bail!("The edited file does not describe the target");
    }
    Ok(())
}

/// Applies the difference between `base` and `updated` to `original`.
fn apply_yaml_change(original: &str, base: &str, updated: &str) -> Result<String> {
    let base: Value = yaml_serde::from_str(base).context("Failed to parse the base")?;
    let new: Value = yaml_serde::from_str(updated).context("Failed to parse the update")?;

    // `YamlFile` keeps the comments before the document start, `Document` rejects them.
    let file = YamlFile::from_str(original).context("Failed to parse the original")?;
    let new_file = YamlFile::from_str(updated).context("Failed to parse the update")?;
    let (Some(node), Some(new_node), Value::Mapping(base), Value::Mapping(new)) = (
        file.document().and_then(|doc| doc.as_mapping()),
        new_file.document().and_then(|doc| doc.as_mapping()),
        &base,
        &new,
    ) else {
        bail!("A target description must be a mapping");
    };

    update_mapping(&node, &new_node, base, new);

    Ok(file.to_string())
}

/// Edits `node` in place. Returns `false` if the caller must replace it with `new_node`.
fn update_node(node: &YamlNode, new_node: &YamlNode, base: &Value, new: &Value) -> bool {
    match (node, new_node, base, new) {
        (
            YamlNode::TaggedNode(node),
            YamlNode::TaggedNode(new_node),
            Value::Tagged(base),
            Value::Tagged(new),
        ) if base.tag == new.tag => {
            let (Some(node), Some(new_node), Value::Mapping(base), Value::Mapping(new)) = (
                node.as_mapping(),
                new_node.as_mapping(),
                &base.value,
                &new.value,
            ) else {
                return false;
            };
            update_mapping(&node, &new_node, base, new);
        }
        (
            YamlNode::Mapping(node),
            YamlNode::Mapping(new_node),
            Value::Mapping(base),
            Value::Mapping(new),
        ) => update_mapping(node, new_node, base, new),
        (
            YamlNode::Sequence(node),
            YamlNode::Sequence(new_node),
            Value::Sequence(base),
            Value::Sequence(new),
        ) => update_sequence(node, new_node, base, new),
        _ => return false,
    }
    true
}

fn update_mapping(
    node: &Mapping,
    new_node: &Mapping,
    base: &yaml_serde::Mapping,
    new: &yaml_serde::Mapping,
) {
    let mut previous_key = None;
    for (key, new_field) in new {
        let Some(key) = key.as_str() else {
            continue;
        };
        if base.get(key) != Some(new_field) {
            let new_field_node = new_node.get(key).unwrap();
            update_field(
                node,
                previous_key,
                key,
                &new_field_node,
                base.get(key),
                new_field,
            );
        }
        if node.contains_key(key) {
            previous_key = Some(key);
        }
    }

    for key in base.keys().filter_map(Value::as_str) {
        if !new.contains_key(key) {
            node.remove(key);
        }
    }
}

/// Sets `key` in `node`. A new key goes after `previous_key`, to keep the field order.
fn update_field(
    node: &Mapping,
    previous_key: Option<&str>,
    key: &str,
    new_node: &YamlNode,
    base: Option<&Value>,
    new: &Value,
) {
    let Some(field_node) = node.get(key) else {
        match previous_key {
            Some(previous_key) => {
                node.insert_after(previous_key, key, new_node);
            }
            None => node.insert_at_index(0, key, new_node),
        }
        return;
    };
    if !base.is_some_and(|base| update_node(&field_node, new_node, base, new)) {
        node.set(key, new_node);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Edit {
    /// The base item stays, and takes the value of `new[_]`.
    Update(usize),
    /// `new[_]` is inserted. It is the base item at `from`, if that one moved here.
    Insert(usize, Option<usize>),
    Remove,
}

fn update_sequence(node: &Sequence, new_node: &Sequence, base: &[Value], new: &[Value]) {
    let items: Vec<YamlNode> = (0..base.len()).map(|i| node.get(i).unwrap()).collect();

    // Walk from the end, so that the edits do not move the items still to be edited.
    let mut index = base.len();
    for edit in align(base, new).into_iter().rev() {
        let (base_index, new_index) = match edit {
            Edit::Remove => {
                index -= 1;
                node.remove(index);
                continue;
            }
            Edit::Insert(new_index, None) => {
                node.insert(index, new_node.get(new_index).unwrap());
                continue;
            }
            Edit::Insert(new_index, Some(from)) => {
                node.insert(index, &items[from]);
                (from, new_index)
            }
            Edit::Update(new_index) => {
                index -= 1;
                (index, new_index)
            }
        };
        if base[base_index] == new[new_index] {
            continue;
        }
        let new_item = new_node.get(new_index).unwrap();
        let item = node.get(index).unwrap();
        if !update_node(&item, &new_item, &base[base_index], &new[new_index]) {
            node.set(index, &new_item);
        }
    }
}

/// The `name` of a sequence item, which identifies it when its other fields change.
fn item_name(item: &Value) -> Option<&Value> {
    let item = match item {
        Value::Tagged(tagged) => &tagged.value,
        item => item,
    };
    item.get("name")
}

fn same_item(base: &Value, new: &Value) -> bool {
    match (item_name(base), item_name(new)) {
        (Some(base_name), Some(new_name)) => base_name == new_name,
        _ => base == new,
    }
}

/// Aligns `base` with `new` on the longest common subsequence of the same items. Between two
/// aligned items, the removed and inserted items without a name pair up in order as updates. A
/// removed item that is inserted elsewhere becomes a move. Both keep the layout of the item.
fn align(base: &[Value], new: &[Value]) -> Vec<Edit> {
    let lcs = lcs_lengths(base, new);

    let mut edits = vec![];
    let (mut i, mut j) = (0, 0);
    let (mut removed, mut inserted) = (vec![], vec![]);
    while i < base.len() || j < new.len() {
        match (base.get(i), new.get(j)) {
            (Some(base_item), Some(new_item)) if same_item(base_item, new_item) => {
                close_gap(&mut edits, base, new, &mut removed, &mut inserted);
                edits.push(Edit::Update(j));
                (i, j) = (i + 1, j + 1);
            }
            (Some(_), Some(_)) if lcs[i + 1][j] >= lcs[i][j + 1] => {
                removed.push(i);
                i += 1;
            }
            (Some(_), None) => {
                removed.push(i);
                i += 1;
            }
            _ => {
                inserted.push(j);
                j += 1;
            }
        }
    }
    close_gap(&mut edits, base, new, &mut removed, &mut inserted);
    find_moves(&mut edits, base, new);
    edits
}

/// `lcs[i][j]` is the length of the longest common subsequence of `base[i..]` and `new[j..]`.
fn lcs_lengths(base: &[Value], new: &[Value]) -> Vec<Vec<usize>> {
    let mut lcs = vec![vec![0; new.len() + 1]; base.len() + 1];
    for i in (0..base.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if same_item(&base[i], &new[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    lcs
}

fn close_gap(
    edits: &mut Vec<Edit>,
    base: &[Value],
    new: &[Value],
    removed: &mut Vec<usize>,
    inserted: &mut Vec<usize>,
) {
    // Items with different names are different items.
    let updated = removed
        .iter()
        .zip(inserted.iter())
        .take_while(|&(&i, &j)| {
            let (base_name, new_name) = (item_name(&base[i]), item_name(&new[j]));
            base_name.is_none() || new_name.is_none() || base_name == new_name
        })
        .count();
    edits.extend(inserted.drain(..updated).map(Edit::Update));
    edits.extend(removed.drain(updated..).map(|_| Edit::Remove));
    edits.extend(inserted.drain(..).map(|j| Edit::Insert(j, None)));
    removed.clear();
}

/// Turns an insert of an item that is removed elsewhere into a move.
fn find_moves(edits: &mut [Edit], base: &[Value], new: &[Value]) {
    let mut removed = removed_items(edits);
    let is_move =
        |i: usize, j: usize| item_name(&base[i]).is_some() && same_item(&base[i], &new[j]);
    for edit in edits.iter_mut() {
        let Edit::Insert(j, from @ None) = edit else {
            continue;
        };
        if let Some(position) = removed.iter().position(|&i| is_move(i, *j)) {
            *from = Some(removed.swap_remove(position));
        }
    }
}

/// The indices of the base items that `edits` remove.
fn removed_items(edits: &[Edit]) -> Vec<usize> {
    edits
        .iter()
        .filter(|edit| !matches!(edit, Edit::Insert(..)))
        .enumerate()
        .filter_map(|(i, edit)| (*edit == Edit::Remove).then_some(i))
        .collect()
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use probe_rs_target::{MemoryRegion, RamRegion};

    use super::*;

    const ORIGINAL: &str = "\
# Before the document
name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  # The flash
  - !Nvm
    range:
      start: 0x08000000 # Leading zeros stay
      end: 0x08010000
    cores: [main]
  - !Ram
    name: SRAM
    range:
      start: 0x20000000
      end: 0x20004000
    cores:
    - main
    access:
      execute: false
";

    fn ram(name: &str, range: std::ops::Range<u64>) -> MemoryRegion {
        MemoryRegion::Ram(RamRegion {
            name: Some(name.to_owned()),
            range,
            cores: vec!["main".to_owned()],
            memory_ports: vec![],
            is_alias: false,
            access: None,
        })
    }

    #[test]
    fn apply_change_keeps_the_unchanged_text() {
        let base: ChipFamily = yaml_serde::from_str(ORIGINAL).unwrap();
        let mut updated = base.clone();
        let memory_map = &mut updated.variants[0].memory_map;
        let MemoryRegion::Nvm(flash) = &mut memory_map[0] else {
            unreachable!()
        };
        flash.name = Some("Flash".to_owned());
        flash.cores.push("other".to_owned());
        memory_map[1] = ram("SRAM", 0x2000_0000..0x2000_8000);
        memory_map.push(ram("SRAM2", 0x3000_0000..0x3000_1000));

        let edited = apply_change(ORIGINAL, &base, &updated).unwrap();
        assert_eq!(
            edited,
            "\
# Before the document
name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  # The flash
  - !Nvm
    name: Flash
    range:
      start: 0x08000000 # Leading zeros stay
      end: 0x08010000
    cores: [main, other]
  - !Ram
    name: SRAM
    range:
      start: 0x20000000
      end: 0x20008000
    cores:
    - main
  - !Ram
    name: SRAM2
    range:
      start: 0x30000000
      end: 0x30001000
    cores:
    - main
"
        );
    }

    #[test]
    fn reformat_keeps_comments() {
        let original = "\
# Before the document
name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  - !Ram
    name: SRAM # After the flash
    range:
      start: 0x20000000
      end: 0x20004000
    cores: [main]
    is_alias: false
    access:
      read: true
      boot: false
  # The flash
  - !Nvm
    range:
      start: 0x08000000 # Leading zeros stay
      end: 0x08010000
    cores:
    - main
";
        let family: ChipFamily = yaml_serde::from_str(original).unwrap();

        let reformatted = reformat(original, &family).unwrap();
        assert_eq!(
            reformatted,
            "\
# Before the document
name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  # The flash
  - !Nvm
    range:
      start: 0x08000000 # Leading zeros stay
      end: 0x08010000
    cores:
    - main
  - !Ram
    name: SRAM # After the flash
    range:
      start: 0x20000000
      end: 0x20004000
    cores: [main]
"
        );
    }

    #[test]
    fn reformat_drops_default_stack_overflow_check() {
        let original = "\
name: Test Family
variants:
- name: Test Chip
  cores:
  - name: main
    type: armv8m
    core_access_options: !Arm
      ap: !v1 0
  memory_map:
  - !Nvm
    range:
      start: 0x08000000
      end: 0x08010000
    cores:
    - main
  flash_algorithms:
  - algo
flash_algorithms:
- name: algo
  description: Test algorithm
  pc_program_page: 0x1
  flash_properties:
    address_range:
      start: 0x08000000
      end: 0x08010000
    page_size: 0x400
    erased_byte_value: 0xff
    program_page_timeout: 100
    erase_sector_timeout: 1000
  stack_overflow_check: true # The default
";
        let family: ChipFamily = yaml_serde::from_str(original).unwrap();

        let reformatted = reformat(original, &family).unwrap();
        assert_eq!(
            reformatted,
            original.replace("  stack_overflow_check: true # The default\n", "")
        );
    }

    #[test]
    fn reformat_keeps_comments_in_builtin_targets() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let comment_lines = |yaml: &str| yaml.lines().filter(|line| line.contains('#')).count();
        let mut failures = vec![];
        for dir in ["probe-rs/targets", "probe-rs-espressif/targets"] {
            for entry in std::fs::read_dir(workspace.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let original = std::fs::read_to_string(&path).unwrap();
                let family: ChipFamily = yaml_serde::from_str(&original).unwrap();

                let reformatted = match reformat(&original, &family) {
                    Ok(reformatted) => reformatted,
                    Err(error) => {
                        failures.push(format!("{}: {error:#}", path.display()));
                        continue;
                    }
                };
                if comment_lines(&reformatted) != comment_lines(&original) {
                    failures.push(format!("{}: comments lost", path.display()));
                }
                if reformat(&reformatted, &family).ok() != Some(reformatted) {
                    failures.push(format!("{}: second reformat changes it", path.display()));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn apply_change_keeps_comments_in_builtin_targets() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let comment_lines = |yaml: &str| yaml.lines().filter(|line| line.contains('#')).count();
        let mut failures = vec![];
        for dir in ["probe-rs/targets", "probe-rs-espressif/targets"] {
            for entry in std::fs::read_dir(workspace.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let original = std::fs::read_to_string(&path).unwrap();
                let base: ChipFamily = yaml_serde::from_str(&original).unwrap();

                let mut updated = base.clone();
                if let Some(algorithm) = updated.flash_algorithms.first_mut() {
                    algorithm.pc_init = Some(algorithm.pc_init.unwrap_or(0) + 2);
                    algorithm.stack_size = Some(0x1234);
                }
                if let Some(MemoryRegion::Ram(region)) = updated.variants[0].memory_map.first_mut()
                {
                    region.range.end += 0x10;
                }

                match apply_change(&original, &base, &updated) {
                    Ok(edited) if comment_lines(&edited) != comment_lines(&original) => {
                        failures.push(format!("{}: comments lost", path.display()))
                    }
                    Ok(_) => {}
                    Err(error) => failures.push(format!("{}: {error:#}", path.display())),
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
