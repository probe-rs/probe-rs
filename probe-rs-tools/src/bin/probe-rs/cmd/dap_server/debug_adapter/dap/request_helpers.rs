use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::dap_types::{Breakpoint, DisassembledInstruction, Source},
};
use addr2line::gimli::RunTimeEndian;
use anyhow::{Result, anyhow};
use capstone::{
    Endian, arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*,
};
use itertools::Itertools;
use probe_rs::{Core, CoreInterface, CoreType, Error, InstructionSet, MemoryInterface};
use probe_rs_debug::{ColumnType, DebugInfo, SourceLocation};

pub(crate) fn disassemble_target_memory(
    core: &mut Core<'_>,
    debug_info: Option<&DebugInfo>,
    instruction_offset: i64,
    byte_offset: i64,
    memory_reference: u64,
    instruction_count: i64,
) -> Result<Vec<DisassembledInstruction>, DebuggerError> {
    let instruction_set = core.instruction_set()?;
    let core_type = core.core_type();
    let endianness = core.endianness()?;
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

    // 2. We calculate worst-case byte offsets to allow for the requested
    //    instruction offset and count, i.e. we read so far backwards and
    //    forward that we're guaranteed to at least read the requested
    //    offset and count of instructions even if all instructions happen
    //    to be max length instructions.
    let start_memory_offset = start_instruction_offset * max_instruction_size;
    let end_instruction_offset = i64::max(0, instruction_offset + instruction_count).unsigned_abs();
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
        if let Some(di) = debug_info
            && let Some(source_location) = di.get_source_location(start_from_address)
            && let Some(source_address) = source_location.address
        {
            start_from_address = source_address;
        }
    }

    // Ensure pointer alignment (safety measure, should be a no-op).
    start_from_address &= !(min_instruction_size - 1);
    read_until_address &= !(min_instruction_size - 1);

    let cs_le = get_capstone_le(instruction_set, core_type)?;
    let mut code_buffer_le: Vec<u8> = vec![];
    let mut disassembled_instructions: Vec<DisassembledInstruction> = vec![];
    let mut maybe_previous_source_location = None;
    let mut maybe_reference_instruction_index = None;
    let convert_endianness = match debug_info {
        Some(di) => di.endianness() == RunTimeEndian::Big,
        None => endianness == probe_rs::Endian::Big,
    };

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
            fn read_instruction<const N: usize, M>(
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
                HALFWORD => read_instruction::<HALFWORD, _>(
                    &mut read_pointer,
                    core,
                    &mut code_buffer_le,
                    convert_endianness,
                ),
                WORD => read_instruction::<WORD, _>(
                    &mut read_pointer,
                    core,
                    &mut code_buffer_le,
                    convert_endianness,
                ),
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
                presentation_hint: None,
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
                        presentation_hint: None,
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
                if let Some(di) = debug_info
                    && let Some(current_source_location) =
                        di.get_source_location(instruction.address())
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
                            .map(|b| format!("{b:02X}"))
                            .join(" "),
                    ),
                    line,
                    location,
                    symbol: None,
                    presentation_hint: None,
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
        if let Some(inst_with_location) = maybe_inst_with_location
            && let Some(first_instruction) = disassembled_instructions.get_mut(0)
            && first_instruction.location.is_none()
        {
            first_instruction.line = inst_with_location.line;
            first_instruction.column = inst_with_location.column;
            first_instruction.location = inst_with_location.location;
        }
    } else {
        return Err(DebuggerError::Other(anyhow!(
            "<`Disassemble` request: invalid memory reference.>",
        )));
    };
    disassembled_instructions.truncate(instruction_count as usize);

    Ok(disassembled_instructions)
}

pub(crate) fn instruction_breakpoint_response(
    address: u64,
    set_succeeded: bool,
    set_error: Option<&str>,
    source_location: Option<&SourceLocation>,
) -> Breakpoint {
    if set_succeeded {
        let (source, line, column, message) = match source_location {
            Some(loc) => {
                let line = loc.line.map(|l| l as i64);
                let column = loc.column.map(|c| match c {
                    ColumnType::LeftEdge => 0_i64,
                    ColumnType::Column(c) => c as i64,
                });
                let message = Some(format!(
                    "Instruction breakpoint set @:{address:#010x}. File: {}: Line: {}, Column: {}",
                    loc.file_name()
                        .unwrap_or_else(|| "<unknown source file>".to_string()),
                    line.unwrap_or(0),
                    column.unwrap_or(0),
                ));
                (get_dap_source(loc), line, column, message)
            }
            None => (
                None,
                None,
                None,
                Some(format!(
                    "Instruction breakpoint set @:{address:#010x}, but could not resolve a source location."
                )),
            ),
        };
        Breakpoint {
            column,
            end_column: None,
            end_line: None,
            id: Some(address as i64),
            instruction_reference: Some(format!("{address:#010x}")),
            line,
            message,
            offset: None,
            source,
            verified: true,
            reason: None,
        }
    } else {
        Breakpoint {
            column: None,
            end_column: None,
            end_line: None,
            id: None,
            instruction_reference: Some(format!("{address:#010x}")),
            line: None,
            message: Some(match set_error {
                Some(error) => format!(
                    "Warning: Could not set breakpoint at memory address: {address:#010x}: {error}"
                ),
                None => {
                    format!("Warning: Could not set breakpoint at memory address: {address:#010x}")
                }
            }),
            offset: None,
            source: None,
            verified: false,
            reason: None,
        }
    }
}

fn get_capstone_le(
    instruction_set: InstructionSet,
    core_type: CoreType,
) -> Result<Capstone, DebuggerError> {
    let mut cs = match instruction_set {
        InstructionSet::Thumb2 => {
            let mut capstone_builder = Capstone::new()
                .arm()
                .mode(armArchMode::Thumb)
                .endian(Endian::Little);
            if matches!(core_type, CoreType::Armv8m) {
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
        InstructionSet::RV64 => Capstone::new()
            .riscv()
            .mode(riscvArchMode::RiscV64)
            .endian(Endian::Little)
            .build(),
        InstructionSet::RV64C => Capstone::new()
            .riscv()
            .mode(riscvArchMode::RiscV64)
            .endian(Endian::Little)
            .extra_mode(std::iter::once(
                capstone::arch::riscv::ArchExtraMode::RiscVC,
            ))
            .build(),
        InstructionSet::Xtensa => return Err(DebuggerError::Unimplemented),
    }
    .map_err(|err| anyhow!("Error creating capstone: {err:?}"))?;
    let _ = cs.set_skipdata(true);
    Ok(cs)
}

/// A helper function to create a [`Source`] struct from a [`SourceLocation`].
///
/// The path is the build-time path recorded by the compiler in DWARF debug info
/// and refers to a file on the *client's* filesystem. The server emits it
/// verbatim; resolution to an editor buffer is the client's responsibility
/// (correct in both local and `remote_server_mode` deployments). Path rewrites
/// that need knowledge of the user's local toolchain (e.g. mapping the
/// synthetic `/rustc/<hash>/...` prefix on precompiled rustlib paths to the
/// active sysroot) are performed by the VSCode extension, not here.
pub(crate) fn get_dap_source(source_location: &SourceLocation) -> Option<Source> {
    let file_path = source_location.path.to_path();
    let file_name = source_location.file_name();

    Some(Source {
        name: file_name,
        path: Some(file_path.to_string_lossy().to_string()),
        source_reference: None,
        presentation_hint: None,
        origin: None,
        sources: None,
        adapter_data: None,
        checksums: None,
    })
}
