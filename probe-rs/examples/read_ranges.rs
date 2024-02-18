use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use probe_rs::{debug::stack_frame::StackFrameInfo, exception_handler_for_core, CoreDump};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Opt {
    elf_file: PathBuf,
    core_dump: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .pretty()
        .init();

    let opt = Opt::parse();

    let debug_info = probe_rs::debug::DebugInfo::from_file(&opt.elf_file)
        .with_context(|| format!("Failed to open file {:?}", opt.elf_file))?;

    let mut dump = CoreDump::load(&opt.core_dump)
        .with_context(|| format!("Failed to open file {:?}", opt.core_dump))?;

    let initial_registers = dump.debug_registers();
    let exception_handler = exception_handler_for_core(dump.core_type());
    let instruction_set = dump.instruction_set();

    let mut stack_frames = debug_info
        .unwind(
            &mut dump,
            initial_registers,
            exception_handler.as_ref(),
            Some(instruction_set),
        )
        .unwrap();

    let mut all_discrete_memory_ranges = Vec::new();
    // Expand and validate the static and local variables for each stack frame.
    for frame in stack_frames.iter_mut() {
        let mut variable_caches = Vec::new();
        if let Some(static_variables) = &mut frame.static_variables {
            variable_caches.push(static_variables);
        }
        if let Some(local_variables) = &mut frame.local_variables {
            variable_caches.push(local_variables);
        }
        for variable_cache in variable_caches {
            // Cache the deferred top level children of the of the cache.
            variable_cache.recurse_deferred_variables(
                &debug_info,
                &mut dump,
                None,
                1,
                0,
                StackFrameInfo {
                    registers: &frame.registers,
                    frame_base: frame.frame_base,
                    canonical_frame_address: frame.canonical_frame_address,
                },
            );
            all_discrete_memory_ranges.append(&mut variable_cache.get_discrete_memory_ranges());
        }
        // Also capture memory addresses for essential registers.
        for register in frame.registers.0.iter() {
            if let Ok(Some(memory_range)) = register.memory_range() {
                all_discrete_memory_ranges.push(memory_range);
            }
        }
    }

    println!("Found {} ranges", all_discrete_memory_ranges.len());

    Ok(())
}
