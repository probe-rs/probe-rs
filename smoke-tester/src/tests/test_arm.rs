use std::time::Duration;

use colored::Colorize;
use linkme::distributed_slice;
use miette::IntoDiagnostic;
use probe_rs::{
    config::MemoryRegion, probe::DebugProbeError, Architecture, BreakpointCause, Core, CoreStatus,
    Error, HaltReason, MemoryInterface,
};

use crate::{println_test_status, TestFailure, TestResult, TestTracker, CORE_TESTS};

const TEST_CODE: &[u8] = include_bytes!("test_arm.bin");
struct TestCodeContext {
    code_load_address: u64,
}

/// Installs the ARM test code into the target and prepares the core to execute it.
fn setup_test_code(tracker: &TestTracker, core: &mut Core) -> Result<TestCodeContext, TestFailure> {
    if core.architecture() == Architecture::Riscv {
        // Not implemented for RISC-V yet
        return Err(TestFailure::UnimplementedForTarget(
            Box::new(tracker.current_target().clone()),
            "Testing stepping is not implemented for RISC-V yet.".to_string(),
        ));
    }

    let ram_region = core.memory_regions().find_map(MemoryRegion::as_ram_region);

    let ram_region = if let Some(ram_region) = ram_region {
        ram_region.clone()
    } else {
        return Err(TestFailure::Skipped(
            "No RAM configured for core, unable to test stepping".to_string(),
        ));
    };

    core.reset_and_halt(Duration::from_millis(100))
        .into_diagnostic()?;

    let code_load_address = ram_region.range.start;

    core.write_8(code_load_address, TEST_CODE)
        .into_diagnostic()?;

    let registers = core.registers();
    core.write_core_reg(registers.pc().unwrap(), code_load_address)
        .into_diagnostic()?;

    Ok(TestCodeContext { code_load_address })
}

#[distributed_slice(CORE_TESTS)]
fn test_stepping(tracker: &TestTracker, core: &mut Core) -> TestResult {
    println!("Testing stepping...");

    let TestCodeContext {
        code_load_address, ..
    } = setup_test_code(tracker, core)?;
    let registers = core.registers();
    let core_information = core.step().into_diagnostic()?;

    let expected_pc = code_load_address + 2;

    let core_status = core.status().into_diagnostic()?;

    assert_eq!(
        core_information.pc, expected_pc,
        "After stepping, PC should be at 0x{:08x}, but is at 0x{:08x}. Core state: {:?}",
        expected_pc, core_information.pc, core_status
    );
    if core_status != CoreStatus::Halted(HaltReason::Step) {
        log::warn!("Unexpected core status: {:?}!", core_status);
    }

    let r0_value: u64 = core
        .read_core_reg(registers.core_register(0))
        .into_diagnostic()?;

    assert_eq!(r0_value, 0);

    println!("R0 value ok!");

    println!(
        "Core halted at {:#08x}, now trying to run...",
        core_information.pc
    );

    // Run up to the software breakpoint (bkpt) at offset 0x6
    let break_address = code_load_address + 0x6;
    core.run().into_diagnostic()?;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println!("Core did not halt after timeout!");
            core.halt(Duration::from_millis(100)).into_diagnostic()?;

            let pc: u64 = core
                .read_core_reg(core.program_counter())
                .into_diagnostic()?;

            println!("Core stopped at: {pc:#08x}");

            let r2_val: u64 = core
                .read_core_reg(registers.core_register(2))
                .into_diagnostic()?;

            println!("$r2 = {r2_val:#08x}");
        }
        Err(other) => return Err(other.into()),
    }

    println!("Core halted again!");

    let core_status = core.status().into_diagnostic()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core
        .read_core_reg(core.program_counter())
        .into_diagnostic()?;

    assert_eq!(pc, break_address);

    println!("Core halted at {pc:#08x}, now trying to run...");

    // Increase PC by 2 to skip breakpoint.
    core.write_core_reg(core.program_counter(), pc + 2)
        .into_diagnostic()?;

    println!("Run core again, with pc = {:#010x}", pc + 2);

    // Run to the finish
    core.run().into_diagnostic()?;

    // Final breakpoint is at offset 0x10

    let break_address = code_load_address + 0x10;

    match core.wait_for_core_halted(Duration::from_millis(100)) {
        Ok(()) => {}
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            println!("Core did not halt after timeout!");
            core.halt(Duration::from_millis(100)).into_diagnostic()?;

            let pc: u64 = core
                .read_core_reg(core.program_counter())
                .into_diagnostic()?;

            println!("Core stopped at: {pc:#08x}");

            let r2_val: u64 = core
                .read_core_reg(registers.core_register(2))
                .into_diagnostic()?;

            println!("$r2 = {r2_val:#08x}");
        }
        Err(other) => return Err(other.into()),
    }

    let core_status = core.status().into_diagnostic()?;

    assert!(matches!(
        core_status,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let pc: u64 = core
        .read_core_reg(core.program_counter())
        .into_diagnostic()?;

    assert_eq!(pc, break_address, "{pc:#08x} != {break_address:#08x}");

    // Register r2 should be 1 to indicate end of test.
    let r2_val: u64 = core
        .read_core_reg(registers.core_register(2))
        .into_diagnostic()?;
    assert_eq!(1, r2_val);

    Ok(())
}

/// When a target resets, it should persist any breakpoints that were established before the reset.
#[distributed_slice(CORE_TESTS)]
fn test_breakpoint_persistence_across_reset(tracker: &TestTracker, core: &mut Core) -> TestResult {
    println_test_status!(
        tracker,
        blue,
        "Testing breakpoint persistence across reset..."
    );

    let num_breakpoints = core.available_breakpoint_units().into_diagnostic()?;
    if num_breakpoints == 0 {
        return Err(TestFailure::Skipped(
            "This target doesn't have any breakpoints".into(),
        ));
    }

    let TestCodeContext {
        code_load_address, ..
    } = setup_test_code(tracker, core)?;

    let bpt = code_load_address + 4; // Just before the bkpt instruction.

    let run_and_encounter_breakpoint = |core: &mut Core| -> TestResult {
        core.run().into_diagnostic()?;
        core.wait_for_core_halted(Duration::from_millis(100))
            .into_diagnostic()?;

        let core_status = core.status().into_diagnostic()?;

        if !matches!(
            core_status,
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
                | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
        ) {
            return Err(TestFailure::Error(
                format!(
                    "Core status is not properly halted! Instead, it's {:?}",
                    core_status
                )
                .into(),
            ));
        }

        let pc: u64 = core
            .read_core_reg(core.program_counter())
            .into_diagnostic()?;

        if pc != bpt {
            return Err(TestFailure::Error(
                format!("PC should be at {bpt:#010X} but it was at {pc:#010X}").into(),
            ));
        }

        Ok(())
    };

    println!("Setting breakpoint after reset...");
    core.set_hw_breakpoint(bpt).into_diagnostic()?;

    // Disable the breakpoint no matter the (early) result.
    let test_result = || -> TestResult {
        run_and_encounter_breakpoint(core)?;
        println!("Resetting core with previously-enabled breakpoint...");
        setup_test_code(tracker, core)?;
        run_and_encounter_breakpoint(core)?;
        Ok(())
    }();
    let cleanup_result = core.clear_all_hw_breakpoints();
    test_result.and_then(|_| cleanup_result.into_diagnostic().map_err(From::from))
}
