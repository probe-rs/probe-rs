use std::time::Instant;

use anyhow::Context;
use libtest_mimic::Failed;
use linkme::distributed_slice;
use probe_rs::{
    Architecture, Core, CoreInterface, MemoryInterface, Session,
    config::MemoryRegion,
    flashing::{DownloadOptions, FlashProgress, FormatKind, download_file_with_options},
};

pub mod stepping;

use crate::{CORE_TESTS, SESSION_TESTS, TestResult, dut_definition::DutDefinition, skip_test};

#[distributed_slice(CORE_TESTS)]
pub fn test_register_read(dut_definition: &DutDefinition, core: &mut Core) -> TestResult {
    println!("Testing register read...");

    let register = core.registers();

    for register in register.core_registers() {
        let _: u64 = core
            .read_core_reg(register)
            .with_context(|| format!("Failed to read register {}", register.name()))?;
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_register_write(definition: &DutDefinition, core: &mut Core) -> TestResult {
    println!("Testing register write...");

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

fn test_write_read(scenario: &str, core: &mut Core, address: u64, data: &[u8]) -> TestResult {
    println!(
        "Testing: write and read at address {:#010X}: {scenario}",
        address
    );

    core.write(address, data)
        .with_context(|| format!("write to address {address:#010X}"))?;

    let mut read_data = vec![0; data.len()];
    core.read(address, &mut read_data)
        .with_context(|| format!("read from address {address:#010X}"))?;

    assert_eq!(
        data,
        &read_data[..],
        "Error reading back {} bytes from address {:#010X}",
        data.len(),
        address
    );

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_memory_access(dut: &DutDefinition, core: &mut Core) -> TestResult {
    let memory_regions = core
        .memory_regions()
        .filter_map(MemoryRegion::as_ram_region)
        .cloned()
        .collect::<Vec<_>>();

    // Try to write all memory regions
    for ram in memory_regions {
        let ram_start = ram.range.start;
        let ram_size = ram.range.end - ram.range.start;
        println!(
            "Testing region: {} ({:#010X} - {:#010X})",
            ram.name.as_deref().unwrap_or("<unnamed region>"),
            ram.range.start,
            ram.range.end
        );

        println!("Test - RAM Start 32");
        // Write first word
        core.write_word_32(ram_start, 0xababab)?;
        let value = core.read_word_32(ram_start)?;
        assert_eq!(
            value, 0xababab,
            "Error reading back 4 bytes from address {ram_start:#010X}"
        );

        println!("Test - RAM End 32");
        // Write last word
        let addr = ram_start + ram_size - 4;
        core.write_word_32(addr, 0xababac)?;
        let value = core.read_word_32(addr)?;
        assert_eq!(
            value, 0xababac,
            "Error reading back 4 bytes from address {addr:#010X}"
        );

        println!("Test - RAM Start 8");
        // Write first byte
        core.write_word_8(ram_start, 0xac)?;
        let value = core.read_word_8(ram_start)?;
        assert_eq!(
            value, 0xac,
            "Error reading back 1 byte from address {ram_start:#010X}"
        );

        println!("Test - RAM 8 Unaligned");
        let address = ram_start + 1;
        let data = 0x23;
        // Write last byte
        core.write_word_8(address, data)
            .with_context(|| format!("write_word_8 to address {address:#010X}"))?;

        let value = core
            .read_word_8(address)
            .with_context(|| format!("read_word_8 from address {address:#010X}"))?;
        assert_eq!(
            value, data,
            "Error reading back 1 byte from address {address:#010X}"
        );

        println!("Test - RAM End 8");
        // Write last byte
        let address = ram_start + ram_size - 1;
        core.write_word_8(address, 0xcd)
            .with_context(|| format!("write_word_8 to address {address:#010X}"))?;

        let value = core
            .read_word_8(address)
            .with_context(|| format!("read_word_8 from address {address:#010X}"))?;
        assert_eq!(
            value, 0xcd,
            "Error reading back 1 byte from address {address:#010X}"
        );

        test_write_read("1 byte at RAM start", core, ram_start, &[0x56])?;
        test_write_read(
            "4 bytes at RAM start",
            core,
            ram_start,
            &[0x12, 0x34, 0x56, 0x78],
        )?;
        test_write_read(
            "4 bytes at RAM end",
            core,
            ram_start + ram_size - 4,
            &[0x12, 0x34, 0x56, 0x78],
        )?;
    }

    Ok(())
}

#[distributed_slice(CORE_TESTS)]
fn test_hw_breakpoints(definition: &DutDefinition, core: &mut Core) -> TestResult {
    println!("Testing HW breakpoints");

    let memory_regions: Vec<_> = core
        .memory_regions()
        .filter_map(MemoryRegion::as_nvm_region)
        .filter(|r| r.is_executable())
        .cloned()
        .collect();

    if memory_regions.is_empty() {
        skip_test!("No NVM memory regions found, unable to test HW breakpoints.".to_string());
    }

    // For this test, we assume that code is executed from Flash / non-volatile memory, and try to set breakpoints
    // in these regions.
    for region in memory_regions {
        println!(
            "Testing region: {} ({:#010X} - {:#010X})",
            region.name.as_deref().unwrap_or("<unnamed region>"),
            region.range.start,
            region.range.end
        );
        let initial_breakpoint_addr = region.range.start;

        let num_breakpoints = core.available_breakpoint_units()?;

        println!("{} breakpoints supported", num_breakpoints);

        if num_breakpoints == 0 {
            println!("No HW breakpoints supported");
            continue;
        }

        let breakpoint_addresses = (0..num_breakpoints as u64)
            .map(|i| initial_breakpoint_addr + 4 * i)
            .collect::<Vec<_>>();

        // Test CoreInterface
        for (i, address) in breakpoint_addresses.iter().enumerate() {
            CoreInterface::set_hw_breakpoint(core, i, *address)?;
        }

        let breakpoints = core.hw_breakpoints()?;
        for (i, address) in breakpoint_addresses.iter().enumerate() {
            assert_eq!(
                breakpoints[i],
                Some(*address),
                "Error reading back HW breakpoint at index {i}"
            );
        }

        // Now check that breakpoints can be overwritten.
        CoreInterface::set_hw_breakpoint(core, 0, breakpoint_addresses[0] + 4)?;
        let breakpoints = core.hw_breakpoints()?;
        assert_eq!(
            breakpoints[0],
            Some(breakpoint_addresses[0] + 4),
            "Error reading back HW breakpoint at index 0"
        );

        // Clear all breakpoints again
        for i in 0..num_breakpoints {
            CoreInterface::clear_hw_breakpoint(core, i as usize)?;
        }

        // Test inherent methods
        for i in 0..num_breakpoints {
            core.set_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)?;
        }

        // Try to set an additional breakpoint, which should fail
        core.set_hw_breakpoint(initial_breakpoint_addr + num_breakpoints as u64 * 4)
            .expect_err("Trying to use more than supported number of breakpoints should fail.");

        // However, we should be able to update a specific breakpoint
        core.set_hw_breakpoint_unit(0, initial_breakpoint_addr + num_breakpoints as u64 * 4)?;

        // Clear all breakpoints again
        core.clear_hw_breakpoint(initial_breakpoint_addr + num_breakpoints as u64 * 4)?;
        for i in 1..num_breakpoints {
            core.clear_hw_breakpoint(initial_breakpoint_addr + 4 * i as u64)?;
        }
    }

    Ok(())
}

#[distributed_slice(SESSION_TESTS)]
pub fn test_flashing(dut_definition: &DutDefinition, session: &mut Session) -> Result<(), Failed> {
    let Some(test_binary) = dut_definition.flash_test_binary.as_deref() else {
        skip_test!("No flash test binary specified");
    };

    let mut options = DownloadOptions::default();
    options.progress = FlashProgress::new(|event| {
        log::debug!("Flash Event: {event:?}");
        print!(".");
    });

    println!("Starting flashing test");
    println!("Binary: {}", test_binary.display());

    let start_time = Instant::now();

    let format = FormatKind::from_optional(session.target().default_format.as_deref()).unwrap();

    let result = download_file_with_options(session, test_binary, format, options);

    println!();

    if let Err(err) = result {
        return Err(err.into());
    }

    println!("Total time for flashing: {:.2?}", start_time.elapsed());

    println!("Finished flashing");

    Ok(())
}
