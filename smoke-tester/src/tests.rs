use std::{path::Path, time::Instant};

use colored::Colorize;
use probe_rs::{
    config::MemoryRegion,
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format},
    Architecture, Core, MemoryInterface, Session,
};

pub mod stepping;

use anyhow::{Context, Result};

use crate::{println_test_status, TestTracker};

pub fn test_register_access(tracker: &TestTracker, core: &mut Core) -> Result<()> {
    println_test_status!(tracker, blue, "Testing register access...");

    let register = core.registers();

    let mut test_value = 1;

    for register in register.platform_registers() {
        // Skip register x0 on RISCV chips, it's hardwired to zero.
        if core.architecture() == Architecture::Riscv && register.name() == "x0" {
            continue;
        }

        // Write new value

        core.write_core_reg(register.into(), test_value)?;

        let readback: u64 = core.read_core_reg(register)?;

        assert_eq!(
            test_value, readback,
            "Error writing register {:?}, read value does not match written value.",
            register
        );

        test_value = test_value.wrapping_shl(1);
    }

    Ok(())
}

pub fn test_memory_access(
    tracker: &TestTracker,
    core: &mut Core,
    core_name: &str,
    memory_regions: &[MemoryRegion],
) -> Result<()> {
    // Try to write all memory regions
    for region in memory_regions {
        match region {
            probe_rs::config::MemoryRegion::Ram(ram)
                if ram.cores.iter().any(|c| c == core_name) =>
            {
                let ram_start = ram.range.start;
                let ram_size = ram.range.end - ram.range.start;

                println_test_status!(tracker, blue, "Test - RAM Start 32");
                // Write first word
                core.write_word_32(ram_start, 0xababab)?;
                let value = core.read_word_32(ram_start)?;
                assert_eq!(value, 0xababab);

                println_test_status!(tracker, blue, "Test - RAM End 32");
                // Write last word
                core.write_word_32(ram_start + ram_size - 4, 0xababac)?;
                let value = core.read_word_32(ram_start + ram_size - 4)?;
                assert_eq!(value, 0xababac);

                println_test_status!(tracker, blue, "Test - RAM Start 8");
                // Write first byte
                core.write_word_8(ram_start, 0xac)?;
                let value = core.read_word_8(ram_start)?;
                assert_eq!(value, 0xac);

                println_test_status!(tracker, blue, "Test - RAM 8 Unaligned");
                let address = ram_start + 1;
                let data = 0x23;
                // Write last byte
                core.write_word_8(address, data)
                    .with_context(|| format!("Write_word_8 to address {:08x}", address))?;

                let value = core
                    .read_word_8(address)
                    .with_context(|| format!("read_word_8 from address {:08x}", address))?;
                assert_eq!(value, data);

                println_test_status!(tracker, blue, "Test - RAM End 8");
                // Write last byte
                core.write_word_8(ram_start + ram_size - 1, 0xcd)
                    .with_context(|| {
                        format!("Write_word_8 to address {:08x}", ram_start + ram_size - 1)
                    })?;

                let value = core
                    .read_word_8(ram_start + ram_size - 1)
                    .with_context(|| {
                        format!("read_word_8 from address {:08x}", ram_start + ram_size - 1)
                    })?;
                assert_eq!(value, 0xcd);
            }
            // Ignore other types of regions
            _other => {}
        }
    }

    Ok(())
}

pub fn test_hw_breakpoints(
    tracker: &TestTracker,
    core: &mut Core,
    memory_regions: &[MemoryRegion],
) -> Result<()> {
    println_test_status!(tracker, blue, "Testing HW breakpoints");

    // For this test, we assume that code is executed from Flash / non-volatile memory, and try to set breakpoints
    // in these regions.
    for region in memory_regions {
        match region {
            probe_rs::config::MemoryRegion::Nvm(nvm) => {
                let initial_breakpoint_addr = nvm.range.start;

                let num_breakpoints = core.available_breakpoint_units()?;

                println_test_status!(tracker, blue, "{} breakpoints supported", num_breakpoints);

                for i in 0..num_breakpoints {
                    core.set_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)?;
                }

                // Try to set an additional breakpoint, which should fail
                core.set_hw_breakpoint(initial_breakpoint_addr + num_breakpoints as u64 * 4)
                    .expect_err(
                        "Trying to use more than supported number of breakpoints should fail.",
                    );

                // Clear all breakpoints again
                for i in 0..num_breakpoints {
                    core.clear_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)?;
                }
            }

            // Skip other regions
            _other => {}
        }
    }

    Ok(())
}

pub fn test_flashing(
    tracker: &TestTracker,
    session: &mut Session,
    test_binary: &Path,
) -> Result<()> {
    let progress = FlashProgress::new(|event| {
        log::debug!("Flash Event: {:?}", event);
        eprint!(".");
    });

    let mut options = DownloadOptions::default();
    options.progress = Some(&progress);

    println_test_status!(tracker, blue, "Starting flashing test");
    println_test_status!(tracker, blue, "Binary: {}", test_binary.display());

    let start_time = Instant::now();

    download_file_with_options(session, test_binary, Format::Elf, options)?;

    println!();

    println_test_status!(
        tracker,
        blue,
        "Total time for flashing: {:.2?}",
        start_time.elapsed()
    );

    println_test_status!(tracker, blue, "Finished flashing");

    Ok(())
}
