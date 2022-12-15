use std::time::Duration;

use anyhow::Result;
use probe_rs::{
    config::MemoryRegion, Architecture, BreakpointCause, Core, CoreStatus, DebugProbeError, Error,
    HaltReason, MemoryInterface,
};

const TEST_CODE: &[u8] = include_bytes!("test_arm.bin");

pub fn test_stepping(core: &mut Core, memory_regions: &[MemoryRegion]) -> Result<()> {
    println!("Testing stepping...");

    if core.architecture() == Architecture::Riscv {
        // Not implemented for RISCV yet
        return Ok(());
    }

    let ram_region = memory_regions.iter().find_map(|region| match region {
        MemoryRegion::Ram(ram) => Some(ram),
        _ => None,
    });

    let ram_region = if let Some(ram_region) = ram_region {
        ram_region.clone()
    } else {
        anyhow::bail!("No RAM configured for core!");
    };

    core.halt(Duration::from_millis(100))?;

    let code_load_address = ram_region.range.start;

    core.write_8(code_load_address, TEST_CODE)?;

    let registers = core.registers();

    core.write_core_reg(registers.program_counter().into(), code_load_address)?;

    let core_information = core.step()?;

    assert_eq!(core_information.pc, code_load_address + 2);

    let core_status = core.status()?;

    if core_status != CoreStatus::Halted(HaltReason::Step) {
        log::warn!("Unexpected core status: {:?}!", core_status);
    }

    let r0_value: u64 = core.read_core_reg(registers.platform_register(0))?;

    assert_eq!(r0_value, 0);

    println!("R0 value ok!");

    println!(
        "Core halted at {:#08x}, now trying to run...",
        core_information.pc
    );

    // Run up to the software breakpoint (bkpt) at offset 0x6
    let break_address = code_load_address + 0x6;
    core.run()?;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println!("Core did not halt after timeout!");
            core.halt(Duration::from_millis(100))?;

            let pc: u64 = core.read_core_reg(registers.program_counter())?;

            println!("Core stopped at: {:#08x}", pc);

            let r2_val: u64 = core.read_core_reg(registers.platform_register(2))?;

            println!("$r2 = {:#08x}", r2_val);
        }
        Err(other) => anyhow::bail!(other),
    }

    println!("Core halted again!");

    let core_status = core.status()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core.read_core_reg(registers.program_counter())?;

    assert_eq!(pc, break_address);

    println!("Core halted at {:#08x}, now trying to run...", pc);

    // Increase PC by 2 to skip breakpoint.
    core.write_core_reg(registers.program_counter().into(), pc + 2)?;

    println!("Run core again, with pc = {:#010x}", pc + 2);

    // Run to the finish
    core.run()?;

    // Final breakpoint is at offset 0x10

    let break_address = code_load_address + 0x10;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println!("Core did not halt after timeout!");
            core.halt(Duration::from_millis(100))?;

            let pc: u64 = core.read_core_reg(registers.program_counter())?;

            println!("Core stopped at: {:#08x}", pc);

            let r2_val: u64 = core.read_core_reg(registers.platform_register(2))?;

            println!("$r2 = {:#08x}", r2_val);
        }
        Err(other) => anyhow::bail!(other),
    }

    let core_status = core.status()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core.read_core_reg(registers.program_counter())?;

    assert_eq!(pc, break_address, "{:#08x} != {:#08x}", pc, break_address);

    // Register r2 should be 1 to indicate end of test.
    let r2_val: u64 = core.read_core_reg(registers.platform_register(2))?;
    assert_eq!(1, r2_val);

    Ok(())
}
