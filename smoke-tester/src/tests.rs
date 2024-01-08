use std::{path::Path, time::Instant};

use colored::Colorize;
use linkme::distributed_slice;
use probe_rs::{
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format},
    Architecture, Core, MemoryInterface, Session,
};

pub mod stepping;

use anyhow::{Context, Result};

use crate::{println_test_status, TestTracker, CORE_TESTS};

#[distributed_slice(CORE_TESTS)]
pub fn test_register_read(tracker: &TestTracker, core: &mut Core) -> Result<(), probe_rs::Error> {
    println_test_status!(tracker, blue, "Testing register read...");

    let register = core.registers();

    for register in register.core_registers() {
        let _: u64 = core
            .read_core_reg(register)
            .with_context(|| format!("Failed to read register {}", register.name()))?;
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_register_write(tracker: &TestTracker, core: &mut Core) -> Result<(), probe_rs::Error> {
    println_test_status!(tracker, blue, "Testing register write...");

    let register = core.registers();

    let mut test_value = 1;

    for register in register.core_registers() {
        // Skip register x0 on RISC-V chips, it's hardwired to zero.
        if core.architecture() == Architecture::Riscv && register.name() == "x0" {
            continue;
        }

        if core.architecture() == Architecture::Arm {
            match register.name() {
                // TODO: Should this be a part of `core_registers`?
                "EXTRA" => continue,
                // TODO: This does not work on all chips (nRF51822), needs to be investigated.
                "XPSR" => continue,
                _ => (),
            }
        }

        // Write new value

        core.write_core_reg(register, test_value)?;

        let readback: u64 = core.read_core_reg(register)?;

        assert_eq!(
            test_value, readback,
            "Error writing register {register:?}, read value does not match written value."
        );

        test_value = test_value.wrapping_shl(1);
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_memory_access(tracker: &TestTracker, core: &mut Core) -> Result<(), probe_rs::Error> {
    let memory_regions = core.memory_regions().cloned().collect::<Vec<_>>();

    // Try to write all memory regions
    for region in memory_regions {
        match region {
            probe_rs::config::MemoryRegion::Ram(ram) => {
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
                    .with_context(|| format!("Write_word_8 to address {address:08x}"))?;

                let value = core
                    .read_word_8(address)
                    .with_context(|| format!("read_word_8 from address {address:08x}"))?;
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

#[distributed_slice(CORE_TESTS)]
fn test_hw_breakpoints(tracker: &TestTracker, core: &mut Core) -> Result<(), probe_rs::Error> {
    println_test_status!(tracker, blue, "Testing HW breakpoints");

    let memory_regions: Vec<_> = core.memory_regions().cloned().collect();

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
    options.progress = Some(progress);

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
