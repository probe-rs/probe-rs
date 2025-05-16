use std::time::Duration;

use colored::Colorize;
use linkme::distributed_slice;
use probe_rs::{
    Architecture, BreakpointCause, Core, CoreStatus, Error, HaltReason, MemoryInterface,
    config::MemoryRegion, probe::DebugProbeError,
};

use crate::{CORE_TESTS, TestFailure, TestResult, TestTracker, println_test_status};

const TEST_CODE: &[u8] = include_bytes!("test_arm.bin");

#[distributed_slice(CORE_TESTS)]
fn test_stepping(tracker: &TestTracker, core: &mut Core) -> TestResult {
    println_test_status!(tracker, blue, "Testing stepping on core {}...", core.id());

    if core.architecture() != Architecture::Arm {
        // Not implemented for RISC-V yet
        return Err(TestFailure::UnimplementedForTarget(
            Box::new(tracker.current_target().clone()),
            format!(
                "Testing stepping is not implemented for {:?} yet.",
                core.architecture()
            ),
        ));
    }

    let ram_region = core
        .memory_regions()
        .filter_map(MemoryRegion::as_ram_region)
        .find(|r| r.is_executable());

    let Some(ram_region) = ram_region else {
        return Err(TestFailure::Skipped(
            "No RAM configured for core, unable to test stepping".to_string(),
        ));
    };

    let code_load_address = ram_region.range.start;

    core.reset_and_halt(Duration::from_millis(100))?;

    core.write_8(code_load_address, TEST_CODE)?;

    let registers = core.registers();
    core.write_core_reg(registers.pc().unwrap(), code_load_address)?;

    let core_information = core.step()?;

    let expected_pc = code_load_address + 2;

    let core_status = core.status()?;

    assert_eq!(
        core_information.pc, expected_pc,
        "After stepping, PC should be at 0x{:08x}, but is at 0x{:08x}. Core state: {:?}",
        expected_pc, core_information.pc, core_status
    );
    if core_status != CoreStatus::Halted(HaltReason::Step) {
        log::warn!("Unexpected core status: {:?}!", core_status);
    }

    let r0_value: u64 = core.read_core_reg(registers.core_register(0))?;

    assert_eq!(r0_value, 0);

    println_test_status!(tracker, green, "R0 value ok!");

    println_test_status!(
        tracker,
        blue,
        "Core halted at {:#08x}, now trying to run...",
        core_information.pc
    );

    // Run up to the software breakpoint (bkpt) at offset 0x6
    let break_address = code_load_address + 0x6;
    core.run()?;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println_test_status!(tracker, yellow, "Core did not halt after timeout!");
            core.halt(Duration::from_millis(100))?;

            let pc: u64 = core.read_core_reg(core.program_counter())?;

            println_test_status!(tracker, blue, "Core stopped at: {pc:#08x}");

            let r2_val: u64 = core.read_core_reg(registers.core_register(2))?;

            println_test_status!(tracker, blue, "$r2 = {r2_val:#08x}");
        }
        Err(other) => return Err(other.into()),
    }

    println_test_status!(tracker, green, "Core halted again!");

    let core_status = core.status()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core.read_core_reg(core.program_counter())?;

    assert_eq!(pc, break_address);

    println_test_status!(
        tracker,
        blue,
        "Core halted at {pc:#08x}, now trying to run..."
    );

    // Increase PC by 2 to skip breakpoint.
    core.write_core_reg(core.program_counter(), pc + 2)?;

    println_test_status!(tracker, blue, "Run core again, with pc = {:#010x}", pc + 2);

    // Run to the finish
    core.run()?;

    // Final breakpoint is at offset 0x10

    let break_address = code_load_address + 0x10;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println_test_status!(tracker, yellow, "Core did not halt after timeout!");
            core.halt(Duration::from_millis(100))?;

            let pc: u64 = core.read_core_reg(core.program_counter())?;

            println_test_status!(tracker, blue, "Core stopped at: {pc:#08x}");

            let r2_val: u64 = core.read_core_reg(registers.core_register(2))?;

            println_test_status!(tracker, blue, "$r2 = {r2_val:#08x}");
        }
        Err(other) => return Err(other.into()),
    }

    let core_status = core.status()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core.read_core_reg(core.program_counter())?;

    assert_eq!(pc, break_address, "{pc:#08x} != {break_address:#08x}");

    // Register r2 should be 1 to indicate end of test.
    let r2_val: u64 = core.read_core_reg(registers.core_register(2))?;
    assert_eq!(1, r2_val);

    Ok(())
}
