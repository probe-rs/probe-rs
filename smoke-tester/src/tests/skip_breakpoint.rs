use std::time::Duration;

use linkme::distributed_slice;
use probe_rs::{
    Architecture, BreakpointCause, Core, CoreStatus, Error, HaltReason, MemoryInterface,
    config::MemoryRegion, probe::DebugProbeError,
};

use crate::{CORE_TESTS, TestResult, dut_definition::DutDefinition, skip_test};

const TEST_CODE: &[u8] = include_bytes!("test_arm.bin");

const SW_BREAKPOINT_OFFSET: u64 = 0x6;
const FINISH_BREAKPOINT_OFFSET: u64 = 0x10;

fn executable_ram_start(core: &mut Core) -> Option<u64> {
    core.memory_regions()
        .filter_map(MemoryRegion::as_ram_region)
        .find(|r| r.is_executable())
        .map(|r| r.range.start)
}

fn load_test_code(core: &mut Core, code_load_address: u64) -> TestResult {
    core.write_8(code_load_address, TEST_CODE)?;

    let registers = core.registers();
    core.write_core_reg(registers.pc().unwrap(), code_load_address)?;

    Ok(())
}

fn wait_for_halt(core: &mut Core, timeout: Duration) -> TestResult {
    match core.wait_for_core_halted(timeout) {
        Ok(()) => Ok(()),
        Err(Error::Probe(DebugProbeError::Timeout)) => {
            core.halt(timeout)?;
            Ok(())
        }
        Err(other) => Err(other.into()),
    }
}

#[smoke_tester_macros::test(core)]
fn test_continue_from_sw_breakpoint(_definition: &DutDefinition, core: &mut Core) -> TestResult {
    if core.architecture() != Architecture::Arm {
        skip_test!("skip_breakpoint resume tests are only implemented for ARM Cortex-M");
    }

    let Some(code_load_address) = executable_ram_start(core) else {
        skip_test!("No executable RAM configured for core, unable to test skip_breakpoint");
    };

    core.reset_and_halt(Duration::from_millis(100))?;
    load_test_code(core, code_load_address)?;

    core.run()?;
    wait_for_halt(core, Duration::from_millis(100))?;

    let first_break_address = code_load_address + SW_BREAKPOINT_OFFSET;
    let pc: u64 = core.read_core_reg(core.program_counter())?;
    assert_eq!(
        pc, first_break_address,
        "expected halt at first SW BKPT {first_break_address:#010x}, got {pc:#010x}"
    );

    assert!(matches!(
        core.status()?,
        CoreStatus::Halted(HaltReason::Breakpoint(_))
    ));

    // Continue without manually bumping PC. `run()` must skip the BKPT opcode.
    core.run()?;
    wait_for_halt(core, Duration::from_millis(100))?;

    let finish_break_address = code_load_address + FINISH_BREAKPOINT_OFFSET;
    let pc: u64 = core.read_core_reg(core.program_counter())?;
    assert_eq!(
        pc, finish_break_address,
        "expected halt at finish BKPT {finish_break_address:#010x}, got {pc:#010x}"
    );

    assert!(matches!(
        core.status()?,
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
            | CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Unknown))
    ));

    let registers = core.registers();
    let r2: u64 = core.read_core_reg(registers.core_register(2))?;
    assert_eq!(
        r2, 1,
        "test program should set r2 to 1 before the finish BKPT"
    );

    Ok(())
}

#[smoke_tester_macros::test(core)]
fn test_run_on_running_core(_definition: &DutDefinition, core: &mut Core) -> TestResult {
    if core.architecture() != Architecture::Arm {
        skip_test!("skip_breakpoint resume tests are only implemented for ARM Cortex-M");
    }

    let Some(code_load_address) = executable_ram_start(core) else {
        skip_test!("No executable RAM configured for core, unable to test skip_breakpoint");
    };

    core.reset_and_halt(Duration::from_millis(100))?;
    load_test_code(core, code_load_address)?;

    core.run()?;

    assert!(
        !core.status()?.is_halted(),
        "core should be running before the second run() call"
    );

    // Must return without timing out in a spurious debug step.
    core.run()?;

    Ok(())
}
