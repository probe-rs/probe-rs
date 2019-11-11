use std::collections::HashMap;

use probe_rs::{
    collection,
    probe::flash::flasher::{AlgorithmSelectionError, FlashAlgorithm},
    target::{info::ChipInfo, Target, TargetSelectionError},
};

include!(concat!(env!("OUT_DIR"), "/targets.rs"));

pub fn get_built_in_target(name: impl AsRef<str>) -> Result<Target, TargetSelectionError> {
    let name = name.as_ref().to_string().to_ascii_lowercase();
    TARGETS
        .get(&name[..])
        .ok_or(TargetSelectionError::TargetNotFound(name))
        .and_then(|target| Target::new(target).map_err(From::from))
}

pub fn get_built_in_target_by_chip_id(chip_info: &ChipInfo) -> Option<Target> {
    for target in TARGETS.values() {
        let target = Target::new(target).unwrap();
        if target.manufacturer == chip_info.manufacturer && target.part == chip_info.part {
            return Some(target);
        }
    }

    None
}

pub enum SelectionStrategy {
    Name(String),
    ChipInfo(ChipInfo),
}

pub fn select_target(strategy: &SelectionStrategy) -> Result<Target, TargetSelectionError> {
    match strategy {
        SelectionStrategy::Name(name) => match collection::get_target(name) {
            Some(target) => Ok(target),
            None => get_built_in_target(name),
        },
        SelectionStrategy::ChipInfo(chip_info) => {
            get_built_in_target_by_chip_id(&chip_info).ok_or(TargetSelectionError::TargetNotFound(
                format!("No target info found for device: {}", chip_info),
            ))
        }
    }
}

pub fn get_built_in_algorithm(
    name: impl AsRef<str>,
) -> Result<FlashAlgorithm, AlgorithmSelectionError> {
    let name = name.as_ref().to_string();
    FLASH_ALGORITHMS
        .get(&name[..])
        .ok_or(AlgorithmSelectionError::AlgorithmNotFound(name))
        .and_then(|definition| FlashAlgorithm::new(definition).map_err(From::from))
}

pub fn select_algorithm(name: impl AsRef<str>) -> Result<FlashAlgorithm, AlgorithmSelectionError> {
    let algorithm = match collection::get_algorithm(name.as_ref()) {
        Some(algorithm) => Some(algorithm),
        None => None,
    };
    match algorithm {
        Some(algorithm) => Ok(algorithm),
        None => get_built_in_algorithm(name),
    }
}
