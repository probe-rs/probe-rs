use crate::config::{
    memory::{MemoryRegion, FlashRegion, RamRegion },
    flash_algorithm::FlashAlgorithm,
    chip::Chip,
};

include!(concat!(env!("OUT_DIR"), "/targets.rs"));