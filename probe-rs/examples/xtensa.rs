//! This example demonstrates how to use the implemented parts of the Xtensa interface.

use std::time::Duration;

use anyhow::Result;
use probe_rs::{
    architecture::xtensa::arch::{instruction::Instruction, CpuRegister, SpecialRegister},
    Core, Lister, MemoryInterface, Permissions, Probe,
};

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Get a list of all available debug probes.
    let probe_lister = Lister::new();

    let probes = probe_lister.list_all();

    // Use the first probe found.
    let probe: Probe = probes[0].open(&probe_lister)?;

    let mut session = probe.attach("esp32s3", Permissions::new()).unwrap();

    let mut core = session.core(0).unwrap();

    core.reset_and_halt(Duration::from_millis(500))?;

    // A simple program we can use to step breakpoints and stepping
    let load_addr: u32 = 0x4037_8000;
    let mut program = vec![];

    Instruction::Break(0, 0).encode_into_vec(&mut program);
    Instruction::Break(0, 0).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);
    Instruction::Rsr(SpecialRegister::DebugCause, CpuRegister::A11).encode_into_vec(&mut program);

    // Download code
    core.write_8(load_addr as u64, &program)?;

    tracing::info!("Software breakpoints");

    // Set up processor state
    core.write_core_reg(core.program_counter(), load_addr)?;

    core.run()?;
    core.wait_for_core_halted(Duration::from_millis(500))?;

    // Stopping on a breakpoint means the PC points at the breakpoint instruction
    assert_pc_eq(&mut core, 0x40378000);

    tracing::info!("Single stepping");

    // Step and stop on breakpoint
    core.step().unwrap();
    assert_pc_eq(&mut core, 0x40378003);

    // Step through last breakpoint and first RSR
    core.step().unwrap();
    assert_pc_eq(
        &mut core,
        0x40378009, // A small weirdness: step() stepped through the breakpoint that stopped us and then the next instruction
    );

    // Step through next RSR
    core.step().unwrap();
    assert_pc_eq(&mut core, 0x4037800C);

    // Set a breakpoint to some further RSR instruction
    core.set_hw_breakpoint(0x40378015)?;

    core.run()?;
    core.wait_for_core_halted(Duration::from_millis(500))?;

    assert_pc_eq(&mut core, 0x40378015);

    // Leave the user with a working MCU
    core.reset()?;

    Ok(())
}

#[track_caller]
fn assert_pc_eq(core: &mut Core<'_>, b: u32) {
    let pc = core.read_core_reg::<u32>(core.program_counter()).unwrap();
    assert_eq!(pc, b, "Expected PC to be {b:#010x}, was {pc:#010x}");
}
