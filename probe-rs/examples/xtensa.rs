//! This example demonstrates how to use the implemented parts of the Xtensa interface.

use std::time::Duration;

use anyhow::Result;
use probe_rs::{Lister, MemoryInterface, Permissions, Probe};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probe_lister = Lister::new();

    let probes = probe_lister.list_all();

    // Use the first probe found.
    let probe: Probe = probes[0].open(&probe_lister)?;

    let mut session = probe.attach("esp32s3", Permissions::new()).unwrap();

    let mut core = session.core(0).unwrap();

    core.reset_and_halt(Duration::from_millis(500))?;

    const TEST_MEMORY_REGION_START: u64 = 0x600F_E000;
    const TEST_MEMORY_LEN: usize = 100;

    let mut saved_memory = [0; TEST_MEMORY_LEN];
    core.read_8(TEST_MEMORY_REGION_START, &mut saved_memory[..])?;

    // Zero the memory
    core.write_8(TEST_MEMORY_REGION_START, &[0; TEST_MEMORY_LEN])?;

    // Write a test word into memory, unaligned
    core.write_word_32(TEST_MEMORY_REGION_START + 1, 0xDECAFBAD)?;
    let coffee_opinion = core.read_word_32(TEST_MEMORY_REGION_START + 1)?;

    // Write a test word into memory, aligned
    core.write_word_32(TEST_MEMORY_REGION_START + 8, 0xFEEDC0DE)?;
    let aligned_word = core.read_word_32(TEST_MEMORY_REGION_START + 8)?;

    let mut readback = [0; 12];
    core.read(TEST_MEMORY_REGION_START, &mut readback[..])?;

    tracing::info!("coffee_opinion: {:08X}", coffee_opinion);
    tracing::info!("aligned_word: {:08X}", aligned_word);
    tracing::info!("readback: {:X?}", readback);

    tracing::info!("Single stepping");

    tracing::info!(
        "PC: {:X}",
        core.read_core_reg::<u32>(core.program_counter()).unwrap()
    );

    core.step().unwrap();

    tracing::info!(
        "PC: {:X}",
        core.read_core_reg::<u32>(core.program_counter()).unwrap()
    );

    // Restore memory we just overwrote
    core.write(TEST_MEMORY_REGION_START, &saved_memory[..])?;

    Ok(())
}
