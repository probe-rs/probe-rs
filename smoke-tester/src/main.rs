use std::time::Duration;

use probe_rs::{config::MemoryRegion, Core, MemoryInterface};

use crate::dut_definition::DutDefinition;
use anyhow::{bail, Context, Result};

mod dut_definition;

use structopt::StructOpt;

#[derive(StructOpt)]
struct Options {
    path: String,
}

fn main() -> Result<()> {
    let opts = Options::from_args();

    let definitions = DutDefinition::collect(&opts.path)?;

    println!("Found {} target definitions.", definitions.len());

    let num_duts = definitions.len();

    let mut tests_ok = true;

    for (i, definition) in definitions.iter().enumerate() {
        println!("DUT [{}/{}] - Starting tests", i + 1, num_duts,);

        match handle_dut(definition) {
            Ok(()) => {
                println!("DUT [{}/{}] - Tests Passed", i + 1, num_duts,);
            }
            Err(e) => {
                tests_ok = false;

                println!("DUT [{}/{}] - Error message: {:#}", i + 1, num_duts, e);
                println!("DUT [{}/{}] - Tests Failed", i + 1, num_duts,);
            }
        }
    }

    if tests_ok {
        Ok(())
    } else {
        bail!("Not all tests succesful");
    }
}

fn handle_dut(definition: &DutDefinition) -> Result<()> {
    let probe = definition.open_probe()?;

    println!("Probe: {:?}", probe.get_name());
    println!("Chip:  {:?}", &definition.chip.name);

    let mut session = probe
        .attach(definition.chip.clone())
        .context("Failed to attach to chip")?;

    let target = session.target();

    let memory_regions = target.memory_map.clone();

    let cores = session.list_cores();

    for (core_index, core_type) in cores {
        println!("Core {}: {:?}", core_index, core_type);

        let mut core = session.core(core_index)?;

        println!("Halting core..");

        core.reset_and_halt(Duration::from_millis(500))?;

        test_register_access(&mut core)?;

        test_memory_access(&mut core, &memory_regions)?;

        test_hw_breakpoints(&mut core, &memory_regions)?;

        // Ensure core is not running anymore.
        core.reset_and_halt(Duration::from_millis(200))?;
    }

    Ok(())
}

fn test_register_access(core: &mut Core) -> Result<()> {
    println!("Testing register access...");

    let register = core.registers();

    let mut test_value = 1;

    for register in register.registers() {
        // Write new value

        core.write_core_reg(register.into(), test_value)?;

        let readback = core.read_core_reg(register)?;

        assert_eq!(
            test_value, readback,
            "Error writing register {:?}, read value does not match written value.",
            register
        );

        test_value = test_value.wrapping_shl(1);
    }

    Ok(())
}

fn test_memory_access(core: &mut Core, memory_regions: &[MemoryRegion]) -> Result<()> {
    // Try to write all memory regions
    for region in memory_regions {
        match region {
            probe_rs::config::MemoryRegion::Ram(ram) => {
                let ram_start = ram.range.start;
                let ram_size = ram.range.end - ram.range.start;

                println!("Test - RAM Start 32");
                // Write first word
                core.write_word_32(ram_start, 0xababab)?;
                let value = core.read_word_32(ram_start)?;
                assert!(value == 0xababab);

                println!("Test - RAM End 32");
                // Write last word
                core.write_word_32(ram_start + ram_size - 4, 0xababac)?;
                let value = core.read_word_32(ram_start + ram_size - 4)?;
                assert!(value == 0xababac);

                println!("Test - RAM Start 8");
                // Write first byte
                core.write_word_8(ram_start, 0xac)?;
                let value = core.read_word_8(ram_start)?;
                assert!(value == 0xac);

                println!("Test - RAM 8 Unaligned");
                let address = ram_start + 1;
                let data = 0x23;
                // Write last byte
                core.write_word_8(address, data)
                    .with_context(|| format!("Write_word_8 to address {:08x}", address))?;

                let value = core
                    .read_word_8(address)
                    .with_context(|| format!("read_word_8 from address {:08x}", address))?;
                assert!(value == data);

                println!("Test - RAM End 8");
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
                assert!(value == 0xcd);
            }
            // Ignore other types of regions
            _other => {}
        }
    }

    Ok(())
}

fn test_hw_breakpoints(core: &mut Core, memory_regions: &[MemoryRegion]) -> Result<()> {
    println!("Testing HW breakpoints");

    // For this test, we assume that code is executed from Flash / non-volatile memory, and try to set breakpoints
    // in these regions.
    for region in memory_regions {
        match region {
            probe_rs::config::MemoryRegion::Nvm(nvm) => {
                let initial_breakpoint_addr = nvm.range.start;

                let num_breakpoints = core.get_available_breakpoint_units()?;

                println!("{} breakpoints supported", num_breakpoints);

                for i in 0..num_breakpoints {
                    core.set_hw_breakpoint(initial_breakpoint_addr + 4 * i)?;
                }

                // Try to set an additional breakpoint, which should fail
                core.set_hw_breakpoint(initial_breakpoint_addr + num_breakpoints * 4)
                    .expect_err(
                        "Trying to use more than supported number of breakpoints should fail.",
                    );

                // Clear all breakpoints again
                for i in 0..num_breakpoints {
                    core.clear_hw_breakpoint(initial_breakpoint_addr + 4 * i)?;
                }
            }

            // Skip other regions
            _other => {}
        }
    }

    Ok(())
}
