use std::time::Instant;

use colored::Colorize;
use linkme::distributed_slice;
use probe_rs::{
    config::MemoryRegion,
    flashing::{download_file_with_options, DownloadOptions, FlashProgress, Format},
    Architecture, Core, MemoryInterface, Session,
};

pub mod stepping;

use miette::{IntoDiagnostic, Result, WrapErr};

use crate::{println_test_status, TestFailure, TestResult, TestTracker, CORE_TESTS, SESSION_TESTS};

#[distributed_slice(CORE_TESTS)]
pub fn test_register_read(tracker: &TestTracker, core: &mut Core) -> TestResult {
    println_test_status!(tracker, blue, "Testing register read...");

    let register = core.registers();

    for register in register.core_registers() {
        let _: u64 = core
            .read_core_reg(register)
            .into_diagnostic()
            .with_context(|| format!("Failed to read register {}", register.name()))?;
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_register_write(tracker: &TestTracker, core: &mut Core) -> TestResult {
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

        core.write_core_reg(register, test_value)
            .into_diagnostic()?;

        let readback: u64 = core.read_core_reg(register).into_diagnostic()?;

        assert_eq!(
            test_value, readback,
            "Error writing register {register:?}, read value does not match written value."
        );

        test_value = test_value.wrapping_shl(1);
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_memory_access(tracker: &TestTracker, core: &mut Core) -> TestResult {
    let memory_regions = core
        .memory_regions()
        .filter_map(|region| match region {
            MemoryRegion::Ram(ram) => Some(ram),
            _ => None,
        })
        .cloned()
        .collect::<Vec<_>>();

    // Try to write all memory regions
    for ram in memory_regions {
        let ram_start = ram.range.start;
        let ram_size = ram.range.end - ram.range.start;

        println_test_status!(tracker, blue, "Test - RAM Start 32");
        // Write first word
        core.write_word_32(ram_start, 0xababab)?;
        let value = core.read_word_32(ram_start).into_diagnostic()?;
        assert_eq!(
            value, 0xababab,
            "Error reading back 4 bytes from address {:#010X}",
            ram_start
        );

        println_test_status!(tracker, blue, "Test - RAM End 32");
        // Write last word
        let addr = ram_start + ram_size - 4;
        core.write_word_32(addr, 0xababac).into_diagnostic()?;
        let value = core.read_word_32(addr).into_diagnostic()?;
        assert_eq!(
            value, 0xababac,
            "Error reading back 4 bytes from address {:#010X}",
            addr
        );

        println_test_status!(tracker, blue, "Test - RAM Start 8");
        // Write first byte
        core.write_word_8(ram_start, 0xac).into_diagnostic()?;
        let value = core.read_word_8(ram_start).into_diagnostic()?;
        assert_eq!(
            value, 0xac,
            "Error reading back 1 byte from address {:#010X}",
            ram_start
        );

        println_test_status!(tracker, blue, "Test - RAM 8 Unaligned");
        let address = ram_start + 1;
        let data = 0x23;
        // Write last byte
        core.write_word_8(address, data)
            .into_diagnostic()
            .wrap_err_with(|| format!("write_word_8 to address {address:#010X}"))?;

        let value = core
            .read_word_8(address)
            .into_diagnostic()
            .wrap_err_with(|| format!("read_word_8 from address {address:#010X}"))?;
        assert_eq!(
            value, data,
            "Error reading back 1 byte from address {:#010X}",
            address
        );

        println_test_status!(tracker, blue, "Test - RAM End 8");
        // Write last byte
        let address = ram_start + ram_size - 1;
        core.write_word_8(address, 0xcd)
            .into_diagnostic()
            .wrap_err_with(|| format!("write_word_8 to address {address:#010X}"))?;

        let value = core
            .read_word_8(address)
            .into_diagnostic()
            .wrap_err_with(|| format!("read_word_8 from address {address:#010X}"))?;
        assert_eq!(
            value, 0xcd,
            "Error reading back 1 byte from address {:#010X}",
            address
        );
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_hw_breakpoints(tracker: &TestTracker, core: &mut Core) -> TestResult {
    println_test_status!(tracker, blue, "Testing HW breakpoints");

    let memory_regions: Vec<_> = core
        .memory_regions()
        .filter_map(|region| match region {
            MemoryRegion::Nvm(region) => Some(region),
            _ => None,
        })
        .cloned()
        .collect();

    if memory_regions.is_empty() {
        return Err(TestFailure::Skipped(
            "No NVM memory regions found, unable to test HW breakpoints.".to_string(),
        ));
    }

    // For this test, we assume that code is executed from Flash / non-volatile memory, and try to set breakpoints
    // in these regions.
    for region in memory_regions {
        let initial_breakpoint_addr = region.range.start;

        let num_breakpoints = core.available_breakpoint_units().into_diagnostic()?;

        println_test_status!(tracker, blue, "{} breakpoints supported", num_breakpoints);

        for i in 0..num_breakpoints {
            core.set_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)
                .into_diagnostic()?;
        }

        // Try to set an additional breakpoint, which should fail
        core.set_hw_breakpoint(initial_breakpoint_addr + num_breakpoints as u64 * 4)
            .expect_err("Trying to use more than supported number of breakpoints should fail.");

        // Clear all breakpoints again
        for i in 0..num_breakpoints {
            core.clear_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)
                .into_diagnostic()?;
        }
    }

    Ok(())
}

#[distributed_slice(SESSION_TESTS)]
pub fn test_flashing(tracker: &TestTracker, session: &mut Session) -> Result<(), TestFailure> {
    let Some(test_binary) = tracker
        .current_dut_definition()
        .flash_test_binary
        .as_deref()
    else {
        return Err(TestFailure::MissingResource(
            "No flash test binary specified".to_string(),
        ));
    };

    let progress = FlashProgress::new(|event| {
        log::debug!("Flash Event: {:?}", event);
        print!(".");
    });

    let mut options = DownloadOptions::default();
    options.progress = Some(progress);

    println_test_status!(tracker, blue, "Starting flashing test");
    println_test_status!(tracker, blue, "Binary: {}", test_binary.display());

    let start_time = Instant::now();

    let format = match session.target().default_format {
        probe_rs_target::BinaryFormat::Idf => Format::Idf(Default::default()),
        probe_rs_target::BinaryFormat::Raw => Default::default(),
    };

    let result = download_file_with_options(session, test_binary, format, options);

    println!();

    if let Err(err) = result {
        return Err(TestFailure::Error(Box::new(err)));
    }

    println_test_status!(
        tracker,
        blue,
        "Total time for flashing: {:.2?}",
        start_time.elapsed()
    );

    println_test_status!(tracker, blue, "Finished flashing");

    Ok(())
}
