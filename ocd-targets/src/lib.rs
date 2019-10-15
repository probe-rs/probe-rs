use std::collections::HashMap;

use ocd::{
    target::{
        Target,
        TargetSelectionError,
        info::ChipInfo,
    },
    collection,
    probe::flash::flasher::{
        FlashAlgorithm,
        AlgorithmSelectionError,
    },
};

include!(concat!(env!("OUT_DIR"), "/targets.rs"));

pub fn get_built_in_target(name: impl AsRef<str>) -> Result<Target, TargetSelectionError> {
    let name = name.as_ref().to_string().to_ascii_lowercase();
    TARGETS
        .get(&name[..])
        .ok_or(TargetSelectionError::TargetNotFound(name))
        .and_then(|target| Target::new(target).map_err(From::from))
}

pub fn get_built_in_target_by_chip_id(chip_info: &ChipInfo) -> Result<Target, TargetSelectionError> {
    for target in TARGETS.values() {
        match Target::new(target) {
            Ok(target) => {
                if target.manufacturer == chip_info.manufacturer
                && target.part == chip_info.part {
                    return Ok(target);
                }
            },
            Err(_e) => continue,
        }
    }

    Err(TargetSelectionError::CouldNotAutodetect)
}

pub enum SelectionStrategy {
    Name(String),
    ChipInfo(ChipInfo),
}

pub fn select_target(strategy: &SelectionStrategy) -> Result<Target, TargetSelectionError> {
    match strategy {
        SelectionStrategy::Name(name) => {
            let target = match collection::get_target(name) {
                Some(target) => Some(target),
                None => None,
            };
            match target {
                Some(target) => Ok(target),
                None => get_built_in_target(name),
            }
        },
        SelectionStrategy::ChipInfo(chip_info) => get_built_in_target_by_chip_id(&chip_info),
    }
}

pub fn get_built_in_algorithm(name: impl AsRef<str>) -> Result<FlashAlgorithm, AlgorithmSelectionError> {
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