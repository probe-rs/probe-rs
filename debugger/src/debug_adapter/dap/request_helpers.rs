use crate::debug_adapter::dap::dap_types::{DisassembledInstruction, Source};
use crate::debugger::core_data::CoreHandle;
use crate::DebuggerError;
use anyhow::{anyhow, Result};
use capstone::Endian;
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*,
};
use num_traits::Zero;
use probe_rs::debug::ColumnType;
use probe_rs::debug::SourceLocation;
use probe_rs::CoreType;
use probe_rs::InstructionSet;
use probe_rs::MemoryInterface;
use std::time::Duration;

pub(crate) fn disassemble_target_memory(
    target_core: &mut CoreHandle,
    instruction_offset: i64,
    byte_offset: i64,
    memory_reference: u64,
    instruction_count: i64,
) -> Result<Vec<DisassembledInstruction>, DebuggerError> {
    let cs = get_capstone(target_core)?;
    let target_instruction_set = target_core.core.instruction_set()?;
    let instruction_offset_as_bytes = match target_instruction_set {
        InstructionSet::Thumb2 | InstructionSet::RV32C => {
            // Since we cannot guarantee the size of individual instructions, let's assume we will read the 120% of the requested number of 16-bit instructions.
            (instruction_offset
                * target_core
                    .core
                    .instruction_set()?
                    .get_minimum_instruction_size() as i64)
                / 4
                * 5
        }
        InstructionSet::A32 | InstructionSet::A64 | InstructionSet::RV32 => {
            instruction_offset
                * target_core
                    .core
                    .instruction_set()?
                    .get_minimum_instruction_size() as i64
        }
    };
    let mut assembly_lines: Vec<DisassembledInstruction> = vec![];
    let mut code_buffer: Vec<u8> = vec![];
    let mut read_more_bytes = true;
    let mut read_pointer = if byte_offset.is_negative() {
        Some(memory_reference.saturating_sub(byte_offset.unsigned_abs()))
    } else {
        Some(memory_reference.saturating_add(byte_offset as u64))
    };
    read_pointer = if instruction_offset_as_bytes.is_negative() {
        read_pointer
            .and_then(|rp| {
                rp.saturating_sub(instruction_offset_as_bytes.unsigned_abs())
                    .checked_div(4)
            })
            .map(|rp_memory_aligned| rp_memory_aligned * 4)
    } else {
        read_pointer
            .and_then(|rp| {
                rp.saturating_add(instruction_offset_as_bytes as u64)
                    .checked_div(4)
            })
            .map(|rp_memory_aligned| rp_memory_aligned * 4)
    };
    let mut instruction_pointer = if let Some(read_pointer) = read_pointer {
        read_pointer
    } else {
        let error_message = format!("Unable to calculate starting address for disassembly request with memory reference:{memory_reference:#010X}, byte offset:{byte_offset:#010X}, and instruction offset:{instruction_offset:#010X}.");
        return Err(DebuggerError::Other(anyhow!(error_message)));
    };
    let mut stored_source_location = None;
    while assembly_lines.len() < instruction_count as usize {
        if read_more_bytes {
            if let Some(current_read_pointer) = read_pointer {
                // All supported architectures use maximum 32-bit instructions, and require 32-bit memory aligned reads.
                match target_core.core.read_word_32(current_read_pointer) {
                    Ok(new_word) => {
                        // Advance the read pointer for next time we need it.
                        read_pointer =
                            if let Some(valid_read_pointer) = current_read_pointer.checked_add(4) {
                                Some(valid_read_pointer)
                            } else {
                                // If this happens, the next loop will generate "invalid instruction" records.
                                read_pointer = None;
                                continue;
                            };
                        // Update the code buffer.
                        for new_byte in new_word.to_le_bytes() {
                            code_buffer.push(new_byte);
                        }
                    }
                    Err(memory_read_error) => {
                        // If we can't read data at a given address, then create a "invalid instruction" record, and keep trying.
                        assembly_lines.push(DisassembledInstruction {
                            address: format!("{current_read_pointer:#010X}"),
                            column: None,
                            end_column: None,
                            end_line: None,
                            instruction: format!(
                                "<instruction address not readable : {memory_read_error:?}>"
                            ),
                            instruction_bytes: None,
                            line: None,
                            location: None,
                            symbol: None,
                        });
                        read_pointer = Some(current_read_pointer.saturating_add(4));
                        continue;
                    }
                }
            }
        }

        match cs.disasm_all(&code_buffer, instruction_pointer) {
            Ok(instructions) => {
                if num_traits::Zero::is_zero(&instructions.len()) {
                    // The capstone library sometimes returns an empty result set, instead of an Err. Catch it here or else we risk an infinte loop looking for a valid instruction.
                    return Err(DebuggerError::Other(anyhow::anyhow!(
                        "Disassembly encountered unsupported instructions at memory reference {:#010x?}",
                        instruction_pointer
                    )));
                }

                let mut result_instruction = instructions
                    .iter()
                    .map(|instruction| {
                        // Before processing, update the code buffer appropriately
                        code_buffer = code_buffer.split_at(instruction.len()).1.to_vec();

                        // Variable width instruction sets my not use the full `code_buffer`, so we need to read ahead, to ensure we have enough code in the buffer to disassemble the 'widest' of instructions in the instruction set.
                        read_more_bytes = code_buffer.len() < target_instruction_set.get_maximum_instruction_size() as usize;

                        // Move the instruction_pointer for the next read.
                        instruction_pointer += instruction.len() as u64;

                        // Try to resolve the source location for this instruction.
                        // If we find one, we use it ONLY if it is different from the previous one (stored_source_location).
                        // - This helps to reduce visual noise in the VSCode UX, by not displaying the same line of source code multiple times over.
                        // If we do not find a source location, then just return the raw assembly without file/line/column information.
                        let mut location = None;
                        let mut line = None;
                        let mut column = None;
                        if let Some(current_source_location) = target_core
                            .core_data
                            .debug_info
                            .get_source_location(instruction.address()) {
                            if let Some(previous_source_location) = stored_source_location.clone() {
                                if current_source_location != previous_source_location {
                                    location = get_dap_source(&current_source_location);
                                    line = current_source_location.line.map(|line| line as i64);
                                    column = current_source_location.column.map(|col| match col {
                                        ColumnType::LeftEdge => 0_i64,
                                        ColumnType::Column(c) => c as i64,
                                    });
                                    stored_source_location = Some(current_source_location);
                                }
                            } else {
                                    stored_source_location = Some(current_source_location);
                            }
                        } else {
                            // It won't affect the outcome, but log it for completeness.
                            tracing::debug!("The request `Disassemble` could not resolve a source location for memory reference: {:#010}", instruction.address());
                        }

                        // Create the instruction data.
                        DisassembledInstruction {
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
                                instruction.bytes().iter().map(|b| format!("{b:02X} ")).collect(),
                            ),
                            line,
                            location,
                            symbol: None,
                        }
                    })
                    .collect::<Vec<DisassembledInstruction>>();
                assembly_lines.append(&mut result_instruction);
            }
            Err(error) => {
                return Err(DebuggerError::Other(anyhow!(error)));
            }
        };
    }
    // Because we need to read on a 32-bit boundary, there are cases when the requested start address
    // is not the first line.
    if instruction_offset.is_zero()
        && byte_offset.is_zero()
        && assembly_lines
            .first()
            .and_then(|first| {
                if u64::from_str_radix(&first.address[2..], 16).unwrap_or(memory_reference)
                    < memory_reference
                {
                    Some(true)
                } else {
                    None
                }
            })
            .is_some()
    {
        assembly_lines.remove(0);
    }
    // With variable length instructions, we sometimes get one to many instructions
    // (e.g. when we read a 32-bit instruction, but the next two instructions are 16-bits).
    while assembly_lines.len() > instruction_count as usize {
        let _ = assembly_lines.pop();
    }
    Ok(assembly_lines)
}

pub(crate) fn get_capstone(target_core: &mut CoreHandle) -> Result<Capstone, DebuggerError> {
    let mut cs = match target_core.core.instruction_set()? {
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
    }
    .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;
    let _ = cs.set_skipdata(true);
    Ok(cs)
}

/// A helper function to greate a [`dap_types::Source`] struct from a [`SourceLocation`]
pub(crate) fn get_dap_source(source_location: &SourceLocation) -> Option<Source> {
    // Attempt to construct the path for the source code
    source_location.directory.as_ref().map(|path| {
        let mut path = if path.is_relative() {
            if let Ok(current_path) = std::env::current_dir() {
                current_path.join(path)
            } else {
                path.to_owned()
            }
        } else {
            path.to_owned()
        };

        if let Some(file) = &source_location.file {
            path.push(file);
        }

        if path.exists() {
            Source {
                name: source_location.file.clone(),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
                presentation_hint: None,
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            }
        } else {
            Source {
                name: source_location
                    .file
                    .clone()
                    .map(|file_name| format!("<unavailable>: {file_name}")),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
                presentation_hint: Some("deemphasize".to_string()),
                origin: None,
                sources: None,
                adapter_data: None,
                checksums: None,
            }
        }
    })
}

/// Provides halt functionality that is re-used elsewhere, in context of multiple DAP Requests
pub(crate) fn halt_core(
    target_core: &mut probe_rs::Core,
) -> Result<probe_rs::CoreInformation, DebuggerError> {
    match target_core.halt(Duration::from_millis(100)) {
        Ok(cpu_info) => Ok(cpu_info),
        Err(error) => Err(DebuggerError::Other(anyhow!("{}", error))),
    }
}

/// The DAP protocol uses three related values to determine how to invoke the `Variables` request.
/// This function retrieves that information from the `DebugInfo::VariableCache` and returns it as
/// (`variable_reference`, `named_child_variables_cnt`, `indexed_child_variables_cnt`)
pub(crate) fn get_variable_reference(
    parent_variable: &probe_rs::debug::Variable,
    cache: &mut probe_rs::debug::VariableCache,
) -> (i64, i64, i64) {
    if !parent_variable.is_valid() {
        return (0, 0, 0);
    }
    let mut named_child_variables_cnt = 0;
    let mut indexed_child_variables_cnt = 0;
    if let Ok(children) = cache.get_children(Some(parent_variable.variable_key)) {
        for child_variable in children {
            if child_variable.is_indexed() {
                indexed_child_variables_cnt += 1;
            } else {
                named_child_variables_cnt += 1;
            }
        }
    };

    if named_child_variables_cnt > 0 || indexed_child_variables_cnt > 0 {
        (
            parent_variable.variable_key,
            named_child_variables_cnt,
            indexed_child_variables_cnt,
        )
    } else if parent_variable.variable_node_type.is_deferred()
        && parent_variable.get_value(cache) != "()"
    {
        // We have not yet cached the children for this reference.
        // Provide DAP Client with a reference so that it will explicitly ask for children when the user expands it.
        (parent_variable.variable_key, 0, 0)
    } else {
        // Returning 0's allows VSCode DAP Client to behave correctly for frames that have no variables, and variables that have no children.
        (0, 0, 0)
    }
}
