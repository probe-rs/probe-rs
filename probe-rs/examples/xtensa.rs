//! This example demonstrates how to use the implemented parts of the Xtensa interface.

use anyhow::Result;
use probe_rs::config::ScanChainElement;
use probe_rs::{Lister, MemoryInterface, Probe};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probe_lister = Lister::new();

    let probes = probe_lister.list_all();

    // Use the first probe found.
    let mut probe: Probe = probes[0].open(&probe_lister)?;

    probe.set_speed(100)?;
    probe.select_protocol(probe_rs::WireProtocol::Jtag)?;

    // Scan the chain for an esp32s3.
    probe.set_scan_chain(vec![
        ScanChainElement {
            ir_len: Some(5),
            name: Some("main".to_owned()),
        },
        ScanChainElement {
            ir_len: Some(5),
            name: Some("second".to_owned()),
        },
    ])?;
    probe.attach_to_unspecified()?;
    let mut iface = probe.try_into_xtensa_interface().unwrap();

    iface.enter_ocd_mode()?;

    assert!(iface.is_in_ocd_mode()?);

    iface.halt()?;

    const TEST_MEMORY_REGION_START: u64 = 0x600F_E000;
    const TEST_MEMORY_LEN: usize = 100;

    let mut saved_memory = [0; TEST_MEMORY_LEN];
    iface.read(TEST_MEMORY_REGION_START, &mut saved_memory[..])?;

    // Zero the memory
    iface.write(TEST_MEMORY_REGION_START, &[0; TEST_MEMORY_LEN])?;

    // Write a test word into memory, unaligned
    iface.write_word_32(TEST_MEMORY_REGION_START + 1, 0xDECAFBAD)?;
    let coffee_opinion = iface.read_word_32(TEST_MEMORY_REGION_START + 1)?;

    // Write a test word into memory, aligned
    iface.write_word_32(TEST_MEMORY_REGION_START + 8, 0xFEEDC0DE)?;
    let aligned_word = iface.read_word_32(TEST_MEMORY_REGION_START + 8)?;

    let mut readback = [0; 12];
    iface.read(TEST_MEMORY_REGION_START, &mut readback[..])?;

    tracing::info!("coffee_opinion: {:08X}", coffee_opinion);
    tracing::info!("aligned_word: {:08X}", aligned_word);
    tracing::info!("readback: {:X?}", readback);

    // Restore memory we just overwrote
    iface.write(TEST_MEMORY_REGION_START, &saved_memory[..])?;

    iface.leave_ocd_mode()?;

    Ok(())
}
