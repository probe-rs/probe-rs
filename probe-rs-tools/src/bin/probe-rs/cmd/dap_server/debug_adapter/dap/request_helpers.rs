use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::dap_types::{DisassembledInstruction, Source},
    peripherals::svd_cache::{SvdVariableCache, Variable},
    server::{core_data::CoreHandle, session_data::BreakpointType},
};
use addr2line::gimli::RunTimeEndian;
use anyhow::{Result, anyhow};
use capstone::{
    Endian, arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*,
};
use itertools::Itertools;
use probe_rs::{CoreType, Error, InstructionSet, MemoryInterface};
use probe_rs_debug::{ColumnType, ObjectRef, SourceLocation};
use std::{sync::LazyLock, time::Duration};
use typed_path::TypedPathBuf;

use super::dap_types::{Breakpoint, InstructionBreakpoint, MemoryAddress};

// Source file mapping for rustlib, e.g. Some(("/rustc/<hash>", "<sysroot>/lib/rustlib/src/rust"))
// This can be None if rustc is not found or gives bad output
static RUSTLIB_SOURCE_MAP: LazyLock<Option<(TypedPathBuf, TypedPathBuf)>> = LazyLock::new(|| {
    let rustc = rustc_binary();

    // Call rustc --version --verbose to get hash
    let cmd = std::process::Command::new(&rustc)
        .args(["--version", "--verbose"])
        .output()
        .ok()?;
    let stdout = String::from_utf8(cmd.stdout).ok()?;
    let hash = stdout
        .lines()
        .find_map(|line| line.strip_prefix("commit-hash:"))?
        .trim();

    // Call rustc --print sysroot to get the sysroot
    let cmd = std::process::Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let stdout = String::from_utf8(cmd.stdout).ok()?;
    let sysroot = TypedPathBuf::from(stdout.trim());

    // from is always a Unix path, to is a native path
    let from_path = TypedPathBuf::from_unix(format!("/rustc/{hash}/"));
    let to_path = sysroot.join("lib").join("rustlib").join("src").join("rust");

    Some((from_path, to_path))
});

// Find the rustc binary using the same procedure as rust-analyzer
// https://github.com/rust-lang/rust-analyzer/blob/1b283db47f8de1412c851c92bb4ce4ef039ff8ff/editors/code/src/toolchain.ts#L158
fn rustc_binary() -> std::ffi::OsString {
    let rustc = std::ffi::OsStr::new("rustc");
    let extension = std::ffi::OsStr::new(if cfg!(windows) { "exe" } else { "" });
    let rustc_exe = std::path::Path::new(rustc).with_extension(extension);

    // Find rustc using RUSTC environment variable
    if let Some(path) = std::env::var_os("RUSTC") {
        return path;
    }

    // Find rustc on PATH
    if std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths).find(|path| path.join(&rustc_exe).is_file())
        })
        .is_some()
    {
        return std::ffi::OsString::from(rustc);
    }

    // Find rustc in CARGO_HOME or ~/.cargo
    let cargo_home = if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        Some(std::path::PathBuf::from(cargo_home))
    } else {
        directories::UserDirs::new().map(|dir| dir.home_dir().join(".cargo"))
    };
    if let Some(cargo_home) = cargo_home {
        let path = cargo_home.join("bin").join(&rustc_exe);
        if path.is_file() {
            return path.into_os_string();
        }
    }

    // Just return "rustc" as a last resort
    rustc.to_os_string()
}

pub(crate) async fn disassemble_target_memory(
    target_core: &mut CoreHandle<'_>,
    instruction_offset: i64,
    byte_offset: i64,
    memory_reference: u64,
    instruction_count: i64,
) -> Result<Vec<DisassembledInstruction>, DebuggerError> {
    let instruction_set = target_core.core.instruction_set().await?;
    match instruction_set {
        InstructionSet::Thumb2
        | InstructionSet::RV32C
        | InstructionSet::RV32
        | InstructionSet::A32
        | InstructionSet::A64 => (),
        _ => return Err(DebuggerError::Unimplemented), // e.g. Xtensa.
    };

    let min_instruction_size: u64 = instruction_set.get_minimum_instruction_size().into();
    let max_instruction_size: u64 = instruction_set.get_maximum_instruction_size().into();

    // Adjust the requested memory address with the given byte offset.
    let adjusted_memory_reference: u64 = if byte_offset.is_negative() {
        memory_reference.saturating_sub(byte_offset.unsigned_abs())
    } else {
        memory_reference.saturating_add(byte_offset.unsigned_abs())
    };

    // We're asked for a defined number of instructions, but we only can
    // calculate memory offsets in bytes which is a non-trivial conversion
    // in the case of variable length instruction sets. We therefore read
    // the worst case number of instructions and later throw those in
    // excess away:

    // 1. We ensure that we always have the requested memory address in range,
    //    so that we can identify exact instruction counts relative to this reference.
    let start_instruction_offset: u64 = i64::min(instruction_offset, 0).unsigned_abs();
    let end_instruction_offset: u64 =
        i64::max(0, instruction_offset + instruction_count).unsigned_abs();

    // 2. We calculate worst-case byte offsets to allow for the requested
    //    instruction offset and count, i.e. we read so far backwards and
    //    forward that we're guaranteed to at least read the requested
    //    offset and count of instructions even if all instructions happen
    //    to be max length instructions.
    let start_memory_offset = start_instruction_offset * max_instruction_size;
    let end_memory_offset = (end_instruction_offset + 1) * max_instruction_size;
    let mut start_from_address = adjusted_memory_reference.saturating_sub(start_memory_offset);
    let mut read_until_address = adjusted_memory_reference.saturating_add(end_memory_offset);

    let has_variable_length_instructions = min_instruction_size != max_instruction_size;

    if has_variable_length_instructions {
        // Find the closest source location to ensure that we're starting
        // with a well-aligned instruction pointer. Note: Variable
        // length instructions are not necessarily word-aligned, i.e.
        // in the case of ARM Thumbv2, instructions are embedded into
        // a 16-bit halfword stream.
        if let Some(source_location) = target_core
            .core_data
            .debug_info
            .get_source_location(start_from_address)
        {
            if let Some(source_address) = source_location.address {
                start_from_address = source_address;
            }
        }
    }

    // Ensure pointer alignment (safety measure, should be a no-op).
    start_from_address &= !(min_instruction_size - 1);
    read_until_address &= !(min_instruction_size - 1);

    let cs_le = get_capstone_le(target_core).await?;
    let mut code_buffer_le: Vec<u8> = vec![];
    let mut disassembled_instructions: Vec<DisassembledInstruction> = vec![];
    let mut maybe_previous_source_location = None;
    let mut maybe_reference_instruction_index = None;
    let convert_endianness = target_core.core_data.debug_info.endianness() == RunTimeEndian::Big;

    let mut instruction_pointer = start_from_address;
    'instruction_loop: while instruction_pointer < read_until_address {
        if maybe_reference_instruction_index.is_none()
            && instruction_pointer >= adjusted_memory_reference
        {
            // This instruction will be the one that the requested memory
            // reference points to. We'll calculate instruction offsets
            // relative to this index.
            maybe_reference_instruction_index = Some(disassembled_instructions.len() as i64);
        }

        let mut read_pointer = instruction_pointer + code_buffer_le.len() as u64;
        let mut read_error = None;
        while read_error.is_none() && code_buffer_le.len() < max_instruction_size as usize {
            async fn read_instruction<const N: usize, M>(
                ptr: &mut u64,     // read pointer
                mem: &mut M,       // the target's memory interface
                buf: &mut Vec<u8>, // the code buffer to read into
                conv: bool,        // true if endianness conversion is required
            ) -> Option<Error>
            where
                M: MemoryInterface<Error>,
            {
                // We read instructions as a byte array to preserve original endianness
                // independently of host endianness and memory interface implementation.
                let mut data: [u8; N] = [0; N];
                mem.read(*ptr, &mut data)
                    .await
                    .inspect(|_| {
                        if conv {
                            data.reverse()
                        }
                        buf.extend_from_slice(&data);
                        *ptr += N as u64;
                    })
                    .err()
            }

            const HALFWORD: usize = 2;
            const WORD: usize = 4;

            read_error = match min_instruction_size as usize {
                // For 16 bit or variable size instructions we need to read
                // the code as a halfword stream. Reading a full word and
                // then changing endianness would otherwise reverse instruction
                // order or garble partial 32 bit instructions.
                HALFWORD => {
                    read_instruction::<HALFWORD, _>(
                        &mut read_pointer,
                        &mut target_core.core,
                        &mut code_buffer_le,
                        convert_endianness,
                    )
                    .await
                }
                WORD => {
                    read_instruction::<WORD, _>(
                        &mut read_pointer,
                        &mut target_core.core,
                        &mut code_buffer_le,
                        convert_endianness,
                    )
                    .await
                }
                // All supported architectures have either 16 or 32 bit instructions.
                _ => return Err(DebuggerError::Unimplemented),
            };
        }

        if read_error.is_some() {
            // If we can't read data at a given address, then create
            // an "invalid instruction" record, and keep trying.
            disassembled_instructions.push(DisassembledInstruction {
                address: format!("{instruction_pointer:#010X}"),
                column: None,
                end_column: None,
                end_line: None,
                instruction: format!("<instruction address not readable : {read_error:?}>"),
                instruction_bytes: None,
                line: None,
                location: None,
                symbol: None,
            });
            instruction_pointer += min_instruction_size;
            continue 'instruction_loop;
        }

        // We read a single instruction as otherwise capstone will try to make sense
        // of possibly incomplete instructions at the end of the buffer and render those
        // as byte data or other garbage.
        match cs_le.disasm_count(&code_buffer_le, instruction_pointer, 1) {
            // TODO: Deal with mixed ARM/Thumbv2 encoded sources.
            // Note: The DWARF line number state machine isa register (see DWARF5,
            //       section 6.2.2, table 6.3) could be used to that end on a
            //       "per instruction" basis. Capstone allows switching of the
            //       instruction set at runtime, too. DebugInfo::get_source_location()
            //       has access to the DWARF line program.
            Ok(instructions) => {
                if instructions.is_empty() {
                    // The capstone library sometimes returns an empty result set
                    // instead of an Err. Catch it here or else we risk an infinite
                    // loop looking for a valid instruction.
                    disassembled_instructions.push(DisassembledInstruction {
                        address: format!("{instruction_pointer:#010X}"),
                        column: None,
                        end_column: None,
                        end_line: None,
                        instruction: "<unsupported instruction>".to_owned(),
                        instruction_bytes: None,
                        line: None,
                        location: None,
                        symbol: None,
                    });
                    code_buffer_le = code_buffer_le
                        .split_at(min_instruction_size as usize)
                        .1
                        .to_vec();
                    instruction_pointer += min_instruction_size;
                    continue 'instruction_loop;
                }

                let instruction = &instructions[0];

                // Try to resolve the source location for this instruction:
                // - If we find one, we use it only if it is different from the previous one.
                //   This helps to reduce visual noise in the client.
                // - If we do not find a source location, then just return the raw assembly
                //   without file/line/column information.
                let mut location = None;
                let mut line = None;
                let mut column = None;
                if let Some(current_source_location) = target_core
                    .core_data
                    .debug_info
                    .get_source_location(instruction.address())
                {
                    if maybe_previous_source_location.is_none()
                        || maybe_previous_source_location.is_some_and(|previous_source_location| {
                            previous_source_location != current_source_location
                        })
                    {
                        location = get_dap_source(&current_source_location);
                        line = current_source_location.line.map(|line| line as i64);
                        column = current_source_location.column.map(|col| match col {
                            ColumnType::LeftEdge => 0_i64,
                            ColumnType::Column(c) => c as i64,
                        });
                    }

                    maybe_previous_source_location = Some(current_source_location);
                } else {
                    // It won't affect the outcome, but log it for completeness.
                    tracing::debug!(
                        "The request `Disassemble` could not resolve a source location for memory reference: {:#010}",
                        instruction.address()
                    );
                }

                disassembled_instructions.push(DisassembledInstruction {
                    address: format!("{:#010X}", instruction.address()),
                    column,
                    end_column: None,
                    end_line: None,
                    instruction: format!(
                        "{}  {}",
                        instruction.mnemonic().unwrap_or("<unknown>"),
                        instruction.op_str().unwrap_or("")
                    ),
                    instruction_bytes: Some(
                        instruction
                            .bytes()
                            .iter()
                            .map(|b| format!("{:02X}", b))
                            .join(" "),
                    ),
                    line,
                    location,
                    symbol: None,
                });

                code_buffer_le = code_buffer_le.split_at(instruction.len()).1.to_vec();
                instruction_pointer += instruction.len() as u64;
            }
            Err(error) => {
                return Err(DebuggerError::Other(anyhow!(error)));
            }
        };
    }

    // Remove excess instructions
    // at the beginning of the list ...
    if let Some(reference_instruction_index) = maybe_reference_instruction_index {
        let first_instruction_index =
            i64::max(0, reference_instruction_index + instruction_offset) as usize;
        // Keep the last of the removed instructions that had a location
        // and use that location for the first remaining instruction unless
        // the first instruction has a location already.
        let maybe_inst_with_location = disassembled_instructions
            .drain(0..first_instruction_index)
            .rfind(|inst| inst.location.is_some());
        if let Some(inst_with_location) = maybe_inst_with_location {
            if let Some(first_instruction) = disassembled_instructions.get_mut(0) {
                if first_instruction.location.is_none() {
                    first_instruction.line = inst_with_location.line;
                    first_instruction.column = inst_with_location.column;
                    first_instruction.location = inst_with_location.location;
                }
            }
        }
    } else {
        return Err(DebuggerError::Other(anyhow!(
            "<`Disassemble` request: invalid memory reference.>",
        )));
    };
    // ... and at the end of the list.
    disassembled_instructions.truncate(instruction_count as usize);

    Ok(disassembled_instructions)
}

async fn get_capstone_le(target_core: &mut CoreHandle<'_>) -> Result<Capstone, DebuggerError> {
    let mut cs = match target_core.core.instruction_set().await? {
        InstructionSet::Thumb2 => {
            let mut capstone_builder = Capstone::new()
                .arm()
                .mode(armArchMode::Thumb)
                .endian(Endian::Little);
            if matches!(target_core.core.core_type(), CoreType::Armv8m) {
                capstone_builder = capstone_builder
                    .extra_mode(std::iter::once(capstone::arch::arm::ArchExtraMode::V8));
            }
            capstone_builder.build()
        }
        InstructionSet::A32 => Capstone::new()
            .arm()
            .mode(armArchMode::Arm)
            .endian(Endian::Little)
            .build(),
        InstructionSet::A64 => Capstone::new()
            .arm64()
            .mode(aarch64ArchMode::Arm)
            .endian(Endian::Little)
            .build(),
        InstructionSet::RV32 => Capstone::new()
            .riscv()
            .mode(riscvArchMode::RiscV32)
            .endian(Endian::Little)
            .build(),
        InstructionSet::RV32C => Capstone::new()
            .riscv()
            .mode(riscvArchMode::RiscV32)
            .endian(Endian::Little)
            .extra_mode(std::iter::once(
                capstone::arch::riscv::ArchExtraMode::RiscVC,
            ))
            .build(),
        InstructionSet::Xtensa => return Err(DebuggerError::Unimplemented),
    }
    .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;
    let _ = cs.set_skipdata(true);
    Ok(cs)
}

/// A helper function to create a [`Source`] struct from a [`SourceLocation`]
pub(crate) fn get_dap_source(source_location: &SourceLocation) -> Option<Source> {
    let file_path = source_location.path.to_path();

    let file_name = source_location.file_name();

    // Try to convert the path to the native Path of the current OS
    #[cfg(unix)]
    let native_path = file_path.with_unix_encoding_checked().ok()?;
    #[cfg(windows)]
    let native_path = file_path.with_windows_encoding();
    let native_path = std::path::PathBuf::try_from(native_path)
        .map(|mut path| {
            if path.is_relative() {
                if let Ok(current_dir) = std::env::current_dir() {
                    path = current_dir.join(path);
                }
            }
            path
        })
        .ok()?;

    // Check if the source file exists
    if native_path.exists() {
        return Some(Source {
            name: file_name,
            path: Some(native_path.to_string_lossy().to_string()),
            source_reference: None,
            presentation_hint: None,
            origin: None,
            sources: None,
            adapter_data: None,
            checksums: None,
        });
    }

    // Precompiled rustlib paths start with /rustc/<hash>/ which needs to be
    // mapped to <sysroot>/lib/rustlib/src/rust/
    if let Some((old_prefix, new_prefix)) = RUSTLIB_SOURCE_MAP.as_ref() {
        if let Ok(path) = file_path.strip_prefix(old_prefix) {
            if let Ok(rustlib_path) = std::path::PathBuf::try_from(new_prefix.join(path)) {
                if rustlib_path.exists() {
                    return Some(Source {
                        name: file_name,
                        path: Some(rustlib_path.to_string_lossy().to_string()),
                        source_reference: None,
                        presentation_hint: None,
                        origin: None,
                        sources: None,
                        adapter_data: None,
                        checksums: None,
                    });
                }
            }
        }
    }

    // If no matching file was found
    Some(Source {
        name: native_path
            .file_name()
            .map(|file_name| file_name.to_string_lossy().to_string())
            .map(|file_name| format!("<unavailable>: {file_name}")),
        path: Some(file_path.to_string_lossy().to_string()),
        source_reference: None,
        presentation_hint: Some("deemphasize".to_string()),
        origin: None,
        sources: None,
        adapter_data: None,
        checksums: None,
    })
}

/// Provides halt functionality that is re-used elsewhere, in context of multiple DAP Requests
pub(crate) async fn halt_core(
    target_core: &mut probe_rs::Core<'_>,
) -> Result<probe_rs::CoreInformation, DebuggerError> {
    target_core
        .halt(Duration::from_millis(100))
        .await
        .map_err(DebuggerError::from)
}

/// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
/// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
/// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
pub(crate) fn get_variable_reference(
    parent_variable: &probe_rs_debug::Variable,
    cache: &probe_rs_debug::VariableCache,
) -> (ObjectRef, i64, i64) {
    if !parent_variable.is_valid() {
        return (ObjectRef::Invalid, 0, 0);
    }

    let mut named_child_variables_cnt = 0;
    let mut indexed_child_variables_cnt = 0;
    for child_variable in cache.get_children(parent_variable.variable_key()) {
        if child_variable.is_indexed() {
            indexed_child_variables_cnt += 1;
        } else {
            named_child_variables_cnt += 1;
        }
    }

    if named_child_variables_cnt > 0 || indexed_child_variables_cnt > 0 {
        (
            parent_variable.variable_key(),
            named_child_variables_cnt,
            indexed_child_variables_cnt,
        )
    } else if parent_variable.variable_node_type.is_deferred()
        && parent_variable.to_string(cache) != "()"
    {
        // We have not yet cached the children for this reference.
        // Provide DAP Client with a reference so that it will explicitly ask for children when the user expands it.
        (parent_variable.variable_key(), 0, 0)
    } else {
        // Returning 0's allows VSCode DAP Client to behave correctly for frames that have no variables, and variables that have no children.
        (ObjectRef::Invalid, 0, 0)
    }
}

/// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
/// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
/// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
pub(crate) fn get_svd_variable_reference(
    parent_variable: &Variable,
    cache: &SvdVariableCache,
) -> (ObjectRef, i64) {
    let named_child_variables_cnt = cache.get_children(parent_variable.variable_key()).len();

    if named_child_variables_cnt > 0 {
        (
            parent_variable.variable_key(),
            named_child_variables_cnt as i64,
        )
    } else {
        // Returning 0's allows VSCode DAP Client to behave correctly for frames that have no variables, and variables that have no children.
        (ObjectRef::Invalid, 0)
    }
}

/// A helper function to set and return a [`Breakpoint`] struct from a [`InstructionBreakpoint`]
pub(crate) async fn set_instruction_breakpoint(
    requested_breakpoint: InstructionBreakpoint,
    target_core: &mut CoreHandle<'_>,
) -> Breakpoint {
    let mut breakpoint_response = Breakpoint {
        column: None,
        end_column: None,
        end_line: None,
        id: None,
        instruction_reference: None,
        line: None,
        message: None,
        offset: None,
        source: None,
        verified: false,
    };

    if let Ok(MemoryAddress(memory_reference)) = requested_breakpoint
        .instruction_reference
        .as_str()
        .try_into()
    {
        match target_core
            .set_breakpoint(memory_reference, BreakpointType::InstructionBreakpoint)
            .await
        {
            Ok(_) => {
                breakpoint_response.verified = true;
                breakpoint_response.instruction_reference =
                    Some(format!("{memory_reference:#010x}"));
                // Try to resolve the source location for this breakpoint.
                match target_core
                    .core_data
                    .debug_info
                    .get_source_location(memory_reference)
                {
                    Some(source_location) => {
                        breakpoint_response.id = Some(memory_reference as i64);
                        breakpoint_response.source = get_dap_source(&source_location);
                        breakpoint_response.line = source_location.line.map(|line| line as i64);
                        breakpoint_response.column = source_location.column.map(|col| match col {
                            ColumnType::LeftEdge => 0_i64,
                            ColumnType::Column(c) => c as i64,
                        });
                        breakpoint_response.message = Some(format!(
                            "Instruction breakpoint set @:{memory_reference:#010x}. File: {}: Line: {}, Column: {}",
                            &source_location
                                .file_name()
                                .unwrap_or_else(|| "<unknown source file>".to_string()),
                            breakpoint_response.line.unwrap_or(0),
                            breakpoint_response.column.unwrap_or(0)
                        ));
                    }
                    None => {
                        breakpoint_response.message = Some(format!(
                            "Instruction breakpoint set @:{memory_reference:#010x}, but could not resolve a source location."
                        ));
                    }
                }
            }
            Err(error) => {
                breakpoint_response.instruction_reference =
                    Some(requested_breakpoint.instruction_reference);
                breakpoint_response.message = Some(format!(
                    "Warning: Could not set breakpoint at memory address: {memory_reference:#010x}: {error}"
                ));
            }
        }
    } else {
        breakpoint_response.instruction_reference =
            Some(requested_breakpoint.instruction_reference.clone());
        breakpoint_response.message = Some(format!(
            "Invalid memory reference specified: {:?}",
            requested_breakpoint.instruction_reference
        ));
    };
    breakpoint_response
}
