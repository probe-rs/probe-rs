//! Debugging support for probe-rs
//!
//! The `debug` module contains various debug functionality, which can be
//! used to implement a debugger based on `probe-rs`.

mod variable;

use crate::{
    core::{Core, RegisterFile},
    MemoryInterface,
};
use num_traits::Zero;
use probe_rs_target::Architecture;
pub use variable::{Variable, VariableCache, VariantRole};

use std::{
    borrow,
    collections::HashMap,
    io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    rc::Rc,
    str::{from_utf8, Utf8Error},
};

use gimli::{
    DebuggingInformationEntry, FileEntry, LineProgramHeader, Location, UnitOffset, UnwindContext,
};
use object::read::{Object, ObjectSection};

#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    #[error("IO Error while accessing debug data")]
    Io(#[from] io::Error),
    #[error("Error accessing debug data")]
    DebugData(#[from] object::read::Error),
    #[error("Error parsing debug data")]
    Parse(#[from] gimli::read::Error),
    #[error("Non-UTF8 data found in debug data")]
    NonUtf8(#[from] Utf8Error),
    #[error("Error using the probe")]
    Probe(#[from] crate::Error),
    #[error(transparent)]
    CharConversion(#[from] std::char::CharTryFromError),
    #[error(transparent)]
    IntConversion(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ColumnType {
    LeftEdge,
    Column(u64),
}

impl From<gimli::ColumnType> for ColumnType {
    fn from(column: gimli::ColumnType) -> Self {
        match column {
            gimli::ColumnType::LeftEdge => ColumnType::LeftEdge,
            gimli::ColumnType::Column(c) => ColumnType::Column(c.get()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StackFrame {
    pub id: u64,
    pub function_name: String,
    pub source_location: Option<SourceLocation>,
    pub registers: Registers,
    pub pc: u32,
}

// TODO: Fix this
// impl std::fmt::Display for StackFrame {
//     fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
//         writeln!(f, "{}: {}", self.id, self.function_name)?;
//         if let Some(si) = &self.source_location {
//             write!(
//                 f,
//                 "\t{}/{}",
//                 si.directory
//                     .as_ref()
//                     .map(|p| p.to_string_lossy())
//                     .unwrap_or_else(|| std::borrow::Cow::from("<unknown dir>")),
//                 si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
//             )?;

//             if let (Some(column), Some(line)) = (si.column, si.line) {
//                 match column {
//                     ColumnType::Column(c) => write!(f, ":{}:{}", line, c)?,
//                     ColumnType::LeftEdge => write!(f, ":{}", line)?,
//                 }
//             }
//         }

//         writeln!(f)?;
//         writeln!(f, "\tVariables:")?;

//         for variable in &self.variables {
//             variable_recurse(variable, 0, f)?;
//         }
//         write!(f, "")
//     }
// }

// fn variable_recurse(
//     variable: &Variable,
//     level: u32,
//     f: &mut std::fmt::Formatter,
// ) -> std::fmt::Result {
//     for _depth in 0..level {
//         write!(f, "   ")?;
//     }
//     let new_level = level + 1;
//     let ret = writeln!(f, "|-> {} \t= {}", variable.name, variable.get_value());
//     if let Some(children) = variable.children.clone() {
//         for variable in &children {
//             variable_recurse(variable, new_level, f)?;
//         }
//     }

//     ret
// }

#[derive(Debug, Clone)]
pub struct Registers {
    register_description: &'static RegisterFile,

    values: HashMap<u32, u32>,

    architecture: Architecture,
}

impl Registers {
    /// Read all registers from the given core.
    pub fn from_core(core: &mut Core) -> Self {
        let register_file = core.registers();

        let num_platform_registers = register_file.platform_registers.len();

        let mut registers = Registers {
            register_description: register_file,
            values: HashMap::new(),
            architecture: core.architecture(),
        };

        for i in 0..num_platform_registers {
            match core.read_core_reg(register_file.platform_register(i)) {
                Ok(value) => registers.values.insert(i as u32, value),
                Err(e) => {
                    log::debug!("Failed to read value for register {}: {}", i, e);
                    None
                }
            };
        }
        registers
    }

    // TODO: These get_ and set_ functions should probably be implemented as Traits, with architecture specific implementations.

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn get_frame_pointer(&self) -> Option<u32> {
        match self.architecture {
            Architecture::Arm => self.values.get(&7).copied(),
            Architecture::Riscv => self.values.get(&8).copied(),
        }
    }
    /// Set the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn set_frame_pointer(&mut self, value: Option<u32>) {
        let register_address = match self.architecture {
            Architecture::Arm => 7,
            Architecture::Riscv => 8,
        };

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    // TODO: FIX Riscv .... PC is a separate register, and NOT r1 (which is the return address)
    pub fn get_program_counter(&self) -> Option<u32> {
        match self.architecture {
            Architecture::Arm => self.values.get(&15).copied(),
            Architecture::Riscv => self.values.get(&1).copied(),
        }
    }
    pub fn set_program_counter(&mut self, value: Option<u32>) {
        let register_address = match self.architecture {
            Architecture::Arm => 15,
            Architecture::Riscv => 1,
        };

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    pub fn get_stack_pointer(&self) -> Option<u32> {
        match self.architecture {
            Architecture::Arm => self.values.get(&13).copied(),
            Architecture::Riscv => self.values.get(&2).copied(),
        }
    }
    pub fn set_stack_pointer(&mut self, value: Option<u32>) {
        let register_address = match self.architecture {
            Architecture::Arm => 13,
            Architecture::Riscv => 2,
        };

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    pub fn get_return_address(&self) -> Option<u32> {
        match self.architecture {
            Architecture::Arm => self.values.get(&14).copied(),
            Architecture::Riscv => self.values.get(&1).copied(),
        }
    }
    pub fn set_return_address(&mut self, value: Option<u32>) {
        let register_address = match self.architecture {
            Architecture::Arm => 14,
            Architecture::Riscv => 1,
        };

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    pub fn get_value_by_dwarf_register_number(&self, register_number: u32) -> Option<u32> {
        self.values.get(&register_number).copied()
    }

    /// Lookup the register name from the RegisterDescriptions.
    pub fn get_name_by_dwarf_register_number(&self, register_number: u32) -> String {
        if let Some(platform_register) = self
            .register_description
            .get_platform_register(register_number as usize)
        {
            platform_register.name().to_string()
        } else {
            format!("r{} - Uninitialized", register_number)
        }
    }

    pub fn set_by_dwarf_register_number(&mut self, register_number: u32, value: Option<u32>) {
        if let Some(value) = value {
            self.values.insert(register_number, value);
        } else {
            self.values.remove(&register_number);
        }
    }

    pub fn registers(&self) -> impl Iterator<Item = (&u32, &u32)> {
        self.values.iter()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceLocation {
    pub line: Option<u64>,
    pub column: Option<ColumnType>,

    pub file: Option<String>,
    pub directory: Option<PathBuf>,
}

/// `StackFrameIterator` stores the information required to iterate through (`::next()`) each of the frames (`StackFrame`) involved in a `DebugInfo::try_unwind()` operation. The most valuable of these are pointers into the core's `DebugInfo`, as well as the register values for the current frame in the stack (per iteration).
pub struct StackFrameIterator<'debuginfo, 'probe, 'core> {
    debug_info: &'debuginfo mut DebugInfo,
    core: &'core mut Core<'probe>,
    unwind_bases: gimli::BaseAddresses,
    /// The `unwind_context` has the potential to be expensive, so storing it here allows it to be re-used for every iteration in the `next()` implementation.
    unwind_context: Box<gimli::UnwindContext<DwarfReader>>,
    /// Register state as updated for every iteration (previous function) of the unwind process.
    unwind_registers: Registers,
}

impl<'debuginfo, 'probe, 'core> StackFrameIterator<'debuginfo, 'probe, 'core> {
    /// In addition to providing the handle for iterating available `StackFrame`s, this function will also populate the `DebugInfo::VariableCache` with available `static` `Variable`s
    pub fn new(
        debug_info: &'debuginfo mut DebugInfo,
        core: &'core mut Core<'probe>,
        address: u64,
    ) -> Self {
        let mut current_registers = Registers::from_core(core);
        if current_registers.get_program_counter().is_none() {
            current_registers.set_program_counter(Some(address as u32));
        }

        let unwind_context: Box<UnwindContext<DwarfReader>> = Box::new(gimli::UnwindContext::new());
        let unwind_bases = gimli::BaseAddresses::default();

        Self {
            debug_info,
            core,
            unwind_bases,
            unwind_context,
            unwind_registers: current_registers,
        }
    }
}

impl<'debuginfo, 'probe, 'core> Iterator for StackFrameIterator<'debuginfo, 'probe, 'core> {
    type Item = StackFrame;

    /// Performs the logical unwind that is managed by the `StackFrameIterator`. The returned `StackFrame` is that of the **callee** (the frame at the top of the logical unwind process ), the ...
    /// - First call to `::next()` returns the 'StackFrame' at the current PC (program counter), and ...
    /// - Each subsequent call to `::next()` will unwind and return the **previous** `StackFrame` in the call stack.
    /// - Every iteration of the `::next()` method will update the `StackFrameIterator` fields as needed, to ensure that the subsequent calls / iterations know how to correctly unwind the **caller** `StackFrame` (where the current iteration was called from)
    /// - Iteration will continue until there are no more frames to unwind.
    ///
    /// [DWARF](https://dwarfstd.org) 6.4.4 - CIE defines the return register address used in the `gimli::RegisterRule` tables for unwind operations. Theoretically, if we encounter a function that has `Undefined` `gimli::RegisterRule` for the return register address, it means we have reached the bottom of the stack OR the function is a 'no return' type of function. I have found actual examples (e.g. local functions) where we get `Undefined` for register rule when we cannot apply this logic. Example 1: local functions in main.rs will have LR rule as `Undefined`. Example 2: main()-> ! that is called from a trampoline will have a valid LR rule.
    /// The iterator will continue until we have an LR register value of `None`. This will be true under the following conditions:
    /// - We encounter a LR register value of 0xFFFFFFFF which is the 'Reset` value for that register.
    /// - We can not intelligently infer a valid LR register value from the other registers.
    /// - We legitimately get an LR register value of 0x0, indicating the bottom of the stack.
    /// - Similarly, certain error conditions encountered in `StackFrameIterator` will also clear the LR register, to prevent further (likely faulty) iterations.
    ///
    /// Note: In addition to populating the `StackFrame`s, this function will also populate the `DebugInfo::VariableCache` with `Variable`s for available Registers and function variables.
    fn next(&mut self) -> Option<Self::Item> {
        self.unwind_registers.get_return_address()?;
        // PART 0: If we've encountered an error in the previous iteration, the `PC` will be `None`.
        let frame_pc = if let Some(frame_pc) = self.unwind_registers.get_program_counter() {
            frame_pc as u64
        } else {
            log::debug!(
                "UNWIND: No available PC (program counter). Cannot continue stack unwinding."
            );
            return None;
        };

        // PART 1: Construct the `StackFrame` for the current pc.
        log::debug!(
            "\nUNWIND: Will generate `StackFrame` for function at address (PC) {:#010x}",
            frame_pc,
        );

        // PART 1-a: Prepare the `StackFrame` that holds the current frame information
        let return_frame =
            match self
                .debug_info
                .get_stackframe_info(self.core, frame_pc, &self.unwind_registers)
            {
                Ok(frame) => {
                    // self.frame_count += 1;
                    frame
                }
                Err(e) => {
                    log::error!("UNWIND: Unable to complete `StackFrame` information: {}", e);
                    // There is no point in continuing with the unwind, so let's get out of here.
                    self.unwind_registers.set_return_address(None);
                    return None;
                }
            };

        // Part 1-b: When we encounter the starting (after reset) return address, we've reached the bottom of the stack, so no more unwinding after this ...
        if let Some(check_return_address) = self.unwind_registers.get_return_address() {
            if check_return_address == u32::MAX {
                self.unwind_registers.set_return_address(None);
                log::debug!(
                    "UNWIND: Stack unwind complete - Reached the 'Reset' value of the LR register."
                );
                return Some(return_frame);
            }
        }

        // PART 2: Setup the registers for the `next()` iteration (a.k.a. unwind previous frame, a.k.a. "callee", in the call stack).
        log::debug!("\nUNWIND Registers for previous function ...");
        if let Some(unwind_return_address) = self.unwind_registers.get_return_address() {
            // Part2-a: we have to check if the `return_frame::registers` had the LR value modified. This would indicate that it processed an INLINED function, and the unwind process below will take a different path than the one for NON-INLINED functions.
            if let Some(stackframe_return_address) = return_frame.registers.get_return_address() {
                if unwind_return_address != stackframe_return_address {
                    log::debug!("UNWIND - Preparing `StackFrameIterator` to unwind INLINED function {:?} at {:?}",return_frame.function_name,return_frame.source_location);
                    // The only `unwind` we need to do, is to update the PC with the call site address of the inline function. The `StackFrameIterator::next()` iteration will then create a virtual `StackFrame` for the call-site.
                    let register_number = self
                        .unwind_registers
                        .register_description
                        .program_counter()
                        .address
                        .0 as u32;
                    log::debug!(
                        "UNWIND - {:04}: Caller: {:#010x}\tCallee: {:#010x}\tRule: {}",
                        self.unwind_registers
                            .get_name_by_dwarf_register_number(register_number),
                        stackframe_return_address,
                        self.unwind_registers
                            .get_value_by_dwarf_register_number(register_number)
                            .unwrap_or_default(),
                        "PC= Inlined function LR",
                    );
                    self.unwind_registers
                        .set_program_counter(Some(stackframe_return_address));
                    return Some(return_frame);
                }
            } else {
                // Something happened in `DebugInfo::get_stackframe_info, and the return address was cleared.
                log::debug!("UNWIND - Inline function @ {:#010x} had no return address. No additional unwind information available", frame_pc);
                self.unwind_registers.set_return_address(None);
                return Some(return_frame);
            }

            log::debug!("UNWIND - Preparing `StackFrameIterator` to unwind NON-INLINED function {:?} at {:?}",return_frame.function_name,return_frame.source_location);
            // PART 2-b: get the `gimli::FrameDescriptorEntry` for this address and then the unwind info associated with this row.
            // TODO: The `gimli` docs for this function talks about cases where there might be more than one FDE for a function. Investigate if this affects RUST and how to solve.
            use gimli::UnwindSection;
            let frame_descriptor_entry = match self.debug_info.frame_section.fde_for_address(
                &self.unwind_bases,
                frame_pc,
                gimli::DebugFrame::cie_from_offset,
            ) {
                Ok(frame_descriptor_entry) => frame_descriptor_entry,
                Err(error) => {
                    log::error!(
                        "UNWIND: Error reading FrameDescriptorEntry at PC={:#010x} : {}",
                        frame_pc,
                        error
                    );
                    self.unwind_registers.set_return_address(None);
                    return Some(return_frame);
                }
            };

            match frame_descriptor_entry.unwind_info_for_address(
                &self.debug_info.frame_section,
                &self.unwind_bases,
                &mut self.unwind_context,
                frame_pc,
            ) {
                Ok(unwind_info) => {
                    // Because we will be updating the `self.unwind_registers` with previous frame unwind info, we need to keep a copy of the current frame's registers that can be used to resolve [DWARF](https://dwarfstd.org) expressions.
                    let callee_frame_registers = self.unwind_registers.clone();
                    // PART 2-c: Determine the CFA (canonical frame address) to use for this unwind row.
                    let unwind_cfa = match unwind_info.cfa() {
                        gimli::CfaRule::RegisterAndOffset { register, offset } => {
                            let reg_val = self
                                .unwind_registers
                                .get_value_by_dwarf_register_number(register.0 as u32);
                            match reg_val {
                                Some(reg_val) => {
                                    let unwind_cfa = (i64::from(reg_val) + offset) as u32;
                                    log::debug!(
                                        "UNWIND - CFA : {:#010x}\tRule: {:?}",
                                        unwind_cfa,
                                        unwind_info.cfa()
                                    );
                                    Some(unwind_cfa)
                                }
                                None => {
                                    log::error!("UNWIND: `StackFrameIterator` unable to determine the unwind CFA: Missing value of register {}",register.0);
                                    self.unwind_registers.set_return_address(None);
                                    return Some(return_frame);
                                }
                            }
                        }
                        gimli::CfaRule::Expression(_) => unimplemented!(),
                    };

                    // PART 2-d: Unwind registers for the "previous frame.
                    // TODO: Test for RISCV ... This is only tested for ARM right now.
                    // TODO: Maybe do some cleanup on the `Registerfile` API, to make the following more ergonomic.
                    for register_number in 0..self
                        .unwind_registers
                        .register_description
                        .platform_registers
                        .len() as u32
                    {
                        use gimli::read::RegisterRule::*;

                        let register_rule =
                            unwind_info.register(gimli::Register(register_number as u16));

                        let mut register_rule_string = format!("{:?}", register_rule);

                        let new_value = match register_rule {
                            Undefined => {
                                // In many cases, the DWARF has `Undefined` rules for variables like frame pointer, program counter, etc., so we hard-code some rules here to make sure unwinding can continue. If there is a valid rule, it will bypass these hardcoded ones.
                                match register_number {
                                    _fp if register_number
                                        == self
                                            .unwind_registers
                                            .register_description
                                            .frame_pointer()
                                            .address
                                            .0
                                            as u32 =>
                                    {
                                        register_rule_string =
                                            "FP=CFA (dwarf Undefined)".to_string();
                                        callee_frame_registers.get_frame_pointer()
                                    }
                                    _sp if register_number
                                        == self
                                            .unwind_registers
                                            .register_description
                                            .stack_pointer()
                                            .address
                                            .0
                                            as u32 =>
                                    {
                                        // TODO: ARM Specific - Add rules for RISCV
                                        // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section B.1.4.1: Treat bits [1:0] as `Should be Zero or Preserved`
                                        register_rule_string =
                                            "SP=CFA (dwarf Undefined)".to_string();
                                        unwind_cfa.map(|unwind_cfa| unwind_cfa & !0b11)
                                    }
                                    _lr if register_number
                                        == self
                                            .unwind_registers
                                            .register_description
                                            .return_address()
                                            .address
                                            .0
                                            as u32 =>
                                    {
                                        // This value is used to determine the Undefined PC value, and will be set correctly later on in this method.
                                        register_rule_string =
                                            "LR=current LR (dwarf Undefined)".to_string();
                                        callee_frame_registers.get_return_address()
                                    }
                                    _pc if register_number
                                        == self
                                            .unwind_registers
                                            .register_description
                                            .program_counter()
                                            .address
                                            .0
                                            as u32 =>
                                    {
                                        // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section A5.1.2: We have to clear the last bit to ensure the PC is half-word aligned. (on ARM architecture, when in Thumb state for certain instruction types will set the LSB to 1)
                                        // NOTE: PC = Current instruction + 1 address, so to reverse this from LR return address, we have to subtract 4 bytes
                                        // TODO: Ensure that this operation does not seem to have a negative effect on RISCV.
                                        let address_size =
                                            frame_descriptor_entry.cie().address_size() as u32;
                                        register_rule_string = format!(
                                            "PC=(unwound LR & !0b1) - {} (dwarf Undefined)",
                                            address_size
                                        );
                                        self.unwind_registers.get_return_address().and_then(
                                            |return_address| {
                                                if return_address == u32::MAX {
                                                    // No reliable return is available.
                                                    None
                                                } else if return_address.is_zero() {
                                                    Some(0)
                                                } else {
                                                    Some((return_address - address_size) & !0b1)
                                                }
                                            },
                                        )
                                    }
                                    _ => {
                                        // This will result in the register value being cleared for the previous frame.
                                        None
                                    }
                                }
                            }
                            SameValue => callee_frame_registers
                                .get_value_by_dwarf_register_number(register_number),
                            Offset(address_offset) => {
                                if let Some(unwind_cfa) = unwind_cfa {
                                    let previous_frame_register_address =
                                        i64::from(unwind_cfa) + address_offset;
                                    let mut buff = [0u8; 4];
                                    if let Err(e) = self
                                        .core
                                        .read(previous_frame_register_address as u32, &mut buff)
                                    {
                                        log::error!(
                                                        "UNWIND: Failed to read from address {:#010x} ({} bytes): {}",
                                                        previous_frame_register_address,
                                                        4,
                                                        e
                                                    );
                                        log::error!(
                                            "UNWIND: Rule: Offset {} from address {:#010x}",
                                            address_offset,
                                            unwind_cfa
                                        );
                                        self.unwind_registers.set_return_address(None);
                                        return Some(return_frame);
                                    }
                                    let previous_frame_register_value = u32::from_le_bytes(buff);
                                    Some(previous_frame_register_value as u32)
                                } else {
                                    log::error!("UNWIND: Tried to unwind `RegisterRule` at CFA = None. Please report this as a bug.");
                                    self.unwind_registers.set_return_address(None);
                                    return Some(return_frame);
                                }
                            }
                            //TODO: Implement the remainder of these `RegisterRule`s
                            _ => unimplemented!(),
                        };

                        self.unwind_registers
                            .set_by_dwarf_register_number(register_number, new_value);
                        log::debug!(
                            "UNWIND - {:04}: Caller: {:#010x}\tCallee: {:#010x}\tRule: {}",
                            self.unwind_registers
                                .get_name_by_dwarf_register_number(register_number),
                            self.unwind_registers
                                .get_value_by_dwarf_register_number(register_number)
                                .unwrap_or_default(),
                            callee_frame_registers
                                .get_value_by_dwarf_register_number(register_number)
                                .unwrap_or_default(),
                            register_rule_string,
                        );
                    }
                }
                Err(error) => {
                    log::debug!("UNWIND: Stack unwind complete. No available debug info for program counter {:#x}: {}", frame_pc, error);
                    self.unwind_registers.set_return_address(None);
                    return Some(return_frame);
                }
            };

            // Part 3: In order to set the correct value of the previous frame we need to peek one frame deeper in the stack.
            // NOTE: ARM Specific.
            // TODO: Test on RISCV and fix as needed
            if let Some(previous_frame_pc) = self.unwind_registers.get_program_counter() {
                let previous_frame_descriptor_entry = match self
                    .debug_info
                    .frame_section
                    .fde_for_address(
                        &self.unwind_bases,
                        previous_frame_pc as u64,
                        gimli::DebugFrame::cie_from_offset,
                    ) {
                    Ok(frame_descriptor_entry) => frame_descriptor_entry,
                    Err(error) => {
                        log::error!("UNWIND: Error reading previous FrameDescriptorEntry at PC={:#010x} : {}", previous_frame_pc, error);
                        self.unwind_registers.set_return_address(None);
                        return Some(return_frame);
                    }
                };

                match previous_frame_descriptor_entry.unwind_info_for_address(
                    &self.debug_info.frame_section,
                    &self.unwind_bases,
                    &mut self.unwind_context,
                    previous_frame_pc as u64,
                ) {
                    Ok(previous_unwind_info) => {
                        let previous_unwind_cfa = match previous_unwind_info.cfa() {
                            gimli::CfaRule::RegisterAndOffset { register, offset } => {
                                let reg_val = self
                                    .unwind_registers
                                    .get_value_by_dwarf_register_number(register.0 as u32);
                                match reg_val {
                                    Some(reg_val) => {
                                        let unwind_cfa = (i64::from(reg_val) + offset) as u32;
                                        log::debug!(
                                            "UNWIND - CFA : {:#010x}\tRule: Previous Function {:?}",
                                            unwind_cfa,
                                            previous_unwind_info.cfa()
                                        );
                                        Some(unwind_cfa)
                                    }
                                    None => {
                                        log::error!(
                                                        "UNWIND: `StackFrameIterator` unable to determine the previous frame unwind CFA: Missing value of register {}",
                                                        register.0
                                                    );
                                        self.unwind_registers.set_return_address(None);
                                        return Some(return_frame);
                                    }
                                }
                            }
                            gimli::CfaRule::Expression(_) => unimplemented!(),
                        };
                        use gimli::read::RegisterRule::*;

                        let return_register_number = previous_frame_descriptor_entry
                            .cie()
                            .return_address_register()
                            .0 as u32;
                        let register_rule = previous_unwind_info
                            .register(gimli::Register(return_register_number as u16));

                        let register_rule_string = format!("{:?}", register_rule);

                        let new_return_value = match register_rule {
                            Undefined => None,
                            SameValue => self
                                .unwind_registers
                                .get_value_by_dwarf_register_number(return_register_number),
                            Offset(address_offset) => {
                                if let Some(unwind_cfa) = previous_unwind_cfa {
                                    let previous_frame_register_address =
                                        i64::from(unwind_cfa) + address_offset;
                                    let mut buff = [0u8; 4];
                                    if let Err(e) = self
                                        .core
                                        .read(previous_frame_register_address as u32, &mut buff)
                                    {
                                        log::error!(
                                                        "UNWIND: Failed to read from address {:#010x} ({} bytes): {}",
                                                        previous_frame_register_address,
                                                        4,
                                                        e
                                                    );
                                        log::error!(
                                            "UNWIND: Rule: Offset {} from address {:#010x}",
                                            address_offset,
                                            unwind_cfa
                                        );
                                        self.unwind_registers.set_return_address(None);
                                        return Some(return_frame);
                                    }
                                    let previous_frame_register_value = u32::from_le_bytes(buff);
                                    Some(previous_frame_register_value as u32)
                                } else {
                                    log::error!("UNWIND: Tried to unwind `RegisterRule` at CFA = None. Please report this as a bug.");
                                    self.unwind_registers.set_return_address(None);
                                    return Some(return_frame);
                                }
                            }
                            //TODO: Implement the remainder of these `RegisterRule`s
                            _ => unimplemented!(),
                        };
                        self.unwind_registers
                            .set_by_dwarf_register_number(return_register_number, new_return_value);
                        log::debug!(
                            "UNWIND - {:04}: Caller: {:#010x}\tRule: Override with previous frame {}",
                            self.unwind_registers
                                .get_name_by_dwarf_register_number(return_register_number),
                            self.unwind_registers
                                .get_value_by_dwarf_register_number(return_register_number)
                                .unwrap_or_default(),
                            register_rule_string,
                        );
                    }
                    Err(error) => {
                        log::debug!("UNWIND: Stack unwind complete. No available debug info for program counter {:#x}: {}",frame_pc, error);
                        self.unwind_registers.set_return_address(None);
                        return Some(return_frame);
                    }
                };
            } else {
                log::error!("UNWIND: Cannot read previous FrameDescriptorEntry without a valid PC");
                self.unwind_registers.set_return_address(None);
                return Some(return_frame);
            }
        } else {
            log::debug!("UNWIND: We have reached the bottom of the stack. This will be the last `StackFrame` returned.");
        }

        Some(return_frame)
    }
}

type GimliReader = gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>;
type GimliAttribute = gimli::Attribute<GimliReader>;

type DwarfReader = gimli::read::EndianRcSlice<gimli::LittleEndian>;

type FunctionDieType<'abbrev, 'unit> =
    gimli::DebuggingInformationEntry<'abbrev, 'unit, GimliReader, usize>;

type UnitIter =
    gimli::DebugInfoUnitHeadersIter<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;

/// Debug information which is parsed from DWARF debugging information.
pub struct DebugInfo {
    dwarf: gimli::Dwarf<DwarfReader>,
    frame_section: gimli::DebugFrame<DwarfReader>,
    /// A cache of all program `Variable`s that are in scope for the current PC (program counter).
    /// It is initialized by `try_unwind()` and ...
    /// It is populated whenever `UnitInfo::process_tree_node() is called to resove DWARF `Variable`s and runtime values. This is done from ...
    /// - `try_unwind()` where we will resolve `static` scoped `Variable`s.
    /// - `StackFrameIterator::next()` as each new stackframe is constructed. This will resolve `register` `Variable`s as well as all function scoped `Variable`s.
    /// - `VariableCache::get_referenced_children()` when it is requested to expand the referenced children of a `Variable`
    /// Note: All program `Variable`s are traversed recursively through their datatypes, until it reaches a base datatype, unless it encounters a datatype that is a pointer to another datatype. These pointers will only be further resolved when a user explicitly requests it.
    pub variable_cache: VariableCache,
}

impl DebugInfo {
    /// Read debug info directly from a ELF file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<DebugInfo, DebugError> {
        let data = std::fs::read(path)?;

        DebugInfo::from_raw(&data)
    }

    /// Parse debug information directly from a buffer containing an ELF file.
    pub fn from_raw(data: &[u8]) -> Result<Self, DebugError> {
        let object = object::File::parse(data)?;

        // Load a section and return as `Cow<[u8]>`.
        let load_section = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            let data = object
                .section_by_name(id.name())
                .and_then(|section| section.uncompressed_data().ok())
                .unwrap_or_else(|| borrow::Cow::Borrowed(&[][..]));

            Ok(gimli::read::EndianRcSlice::new(
                Rc::from(&*data),
                gimli::LittleEndian,
            ))
        };

        // Load all of the sections.
        let dwarf_cow = gimli::Dwarf::load(&load_section)?;

        use gimli::Section;
        let mut frame_section = gimli::DebugFrame::load(load_section)?;

        // To support DWARF v2, where the address size is not encoded in the .debug_frame section,
        // we have to set the address size here.
        // TODO: With current versions of RUST, do we still need to do this?
        frame_section.set_address_size(4);

        Ok(DebugInfo {
            dwarf: dwarf_cow,
            frame_section,
            variable_cache: VariableCache::new(),
        })
    }

    // Get a reference to the private member `dwarf`
    pub fn get_dwarf(&self) -> &gimli::Dwarf<DwarfReader> {
        &self.dwarf
    }

    pub fn function_name(&self, address: u64, find_inlined: bool) -> Option<String> {
        let mut units = self.dwarf.units();

        while let Some(unit_info) = self.get_next_unit_info(&mut units) {
            if let Some(die_cursor_state) = &mut unit_info.get_function_die(address, find_inlined) {
                let function_name = die_cursor_state.function_name(&unit_info);

                if function_name.is_some() {
                    return function_name;
                }
            }
        }

        None
    }

    /// Try get the [`SourceLocation`] for a given address.
    pub fn get_source_location(&self, address: u64) -> Option<SourceLocation> {
        let mut units = self.dwarf.units();

        while let Ok(Some(header)) = units.next() {
            let unit = match self.dwarf.unit(header) {
                Ok(unit) => unit,
                Err(_) => continue,
            };

            let mut ranges = self.dwarf.unit_ranges(&unit).unwrap();

            while let Ok(Some(range)) = ranges.next() {
                if (range.begin <= address) && (address < range.end) {
                    // Get the function name.

                    let ilnp = match unit.line_program.as_ref() {
                        Some(ilnp) => ilnp,
                        None => return None,
                    };

                    let (program, sequences) = ilnp.clone().sequences().unwrap();

                    // Normalize the address.
                    let mut target_seq = None;

                    for seq in sequences {
                        if (seq.start <= address) && (address < seq.end) {
                            target_seq = Some(seq);
                            break;
                        }
                    }

                    target_seq.as_ref()?;

                    let mut previous_row: Option<gimli::LineRow> = None;

                    let mut rows =
                        program.resume_from(target_seq.as_ref().expect("Sequence not found"));

                    while let Ok(Some((header, row))) = rows.next_row() {
                        if row.address() == address {
                            let (file, directory) = self
                                .find_file_and_directory(&unit, header, row.file(header).unwrap())
                                .unwrap();

                            log::debug!("0x{:4x} - {:?}", address, row.isa());

                            return Some(SourceLocation {
                                line: row.line().map(NonZeroU64::get),
                                column: Some(row.column().into()),
                                file,
                                directory,
                            });
                        } else if (row.address() > address) && previous_row.is_some() {
                            let row = previous_row.unwrap();

                            let (file, directory) = self
                                .find_file_and_directory(&unit, header, row.file(header).unwrap())
                                .unwrap();

                            log::debug!("0x{:4x} - {:?}", address, row.isa());

                            return Some(SourceLocation {
                                line: row.line().map(NonZeroU64::get),
                                column: Some(row.column().into()),
                                file,
                                directory,
                            });
                        }
                        previous_row = Some(*row);
                    }
                }
            }
        }
        None
    }

    fn get_units(&self) -> UnitIter {
        self.dwarf.units()
    }

    fn get_next_unit_info(&self, units: &mut UnitIter) -> Option<UnitInfo> {
        while let Ok(Some(header)) = units.next() {
            if let Ok(unit) = self.dwarf.unit(header) {
                return Some(UnitInfo {
                    debug_info: self,
                    unit,
                });
            };
        }
        None
    }

    /// We do not actually resolve the children of `<statics>` automatically, and only create the necessary header in the `VariableCache`.
    /// This allows us to resolve the `<statics>` on demand/lazily, when a user requests it from the debug client.
    /// This saves a lot of overhead when a user only wants to see the `<locals>` or `<registers>` while stepping through code (the most common use cases)
    fn cache_static_variables(
        &self,
        core: &mut Core<'_>,
        unit_info: &UnitInfo,
        stack_frame_registers: &Registers,
        stackframe_root_variable: &Variable,
    ) -> Result<(), DebugError> {
        // Only process statics for this unit header.
        let abbrevs = &unit_info.unit.abbreviations;
        // Navigate the current unit from the header down.
        if let Ok(mut header_tree) = unit_info.unit.header.entries_tree(abbrevs, None) {
            let unit_node = header_tree.root()?;
            let mut static_root_variable = Variable::new(
                unit_info.unit.header.offset().as_debug_info_offset(),
                Some(unit_node.entry().offset()),
            );
            static_root_variable.referenced_node_offset = Some(unit_node.entry().offset());
            static_root_variable.stack_frame_registers = Some(stack_frame_registers.clone());
            static_root_variable.name = "<statics>".to_string();
            static_root_variable = self.variable_cache.cache_variable(
                stackframe_root_variable.variable_key,
                static_root_variable,
                core,
            )?;
        }
        Ok(())
    }

    /// Resolves and then loads all the `Register` variables into the `DebugInfo::VariableCache`.
    fn cache_register_variables(
        &self,
        registers: &Registers,
        stackframe_root_variable: &Variable,
        core: &mut Core<'_>,
    ) -> Result<(), DebugError> {
        let mut register_root_variable = Variable::new(None, None);
        register_root_variable.name = "<registers>".to_string();
        register_root_variable = self.variable_cache.cache_variable(
            stackframe_root_variable.variable_key,
            register_root_variable,
            core,
        )?;

        let mut sorted_registers = registers
            .clone()
            .values
            .into_iter()
            .collect::<Vec<(u32, u32)>>();
        sorted_registers.sort_by_key(|(register_number, _register_value)| *register_number);

        for (register_number, register_value) in sorted_registers {
            let register_variable = Variable {
                variable_key: 0,
                parent_key: register_root_variable.variable_key,
                name: registers.get_name_by_dwarf_register_number(register_number),
                value: format!("{:#010x}", register_value),
                source_location: None,
                type_name: "Platform Register".to_owned(),
                referenced_node_offset: None,
                header_offset: None,
                entries_offset: None,
                stack_frame_registers: None,
                memory_location: 0,
                byte_size: 4,
                member_index: None,
                range_lower_bound: 0,
                range_upper_bound: 0,
                role: VariantRole::NonVariant,
            };
            self.variable_cache.cache_variable(
                register_root_variable.variable_key,
                register_variable,
                core,
            )?;
        }
        Ok(())
    }

    /// Resolves and then loads all the `function` variables into the `DebugInfo::VariableCache`.
    fn cache_function_variables(
        &self,
        core: &mut Core<'_>,
        die_cursor_state: &mut FunctionDie,
        unit_info: &UnitInfo,
        stack_frame_registers: &Registers,
        stackframe_root_variable: &Variable,
    ) -> Result<(), DebugError> {
        let abbrevs = &unit_info.unit.abbreviations;
        let mut tree = unit_info
            .unit
            .header
            .entries_tree(abbrevs, Some(die_cursor_state.function_die.offset()))?;
        let function_node = tree.root()?;

        let mut function_root_variable = Variable::new(
            unit_info.unit.header.offset().as_debug_info_offset(),
            Some(function_node.entry().offset()),
        );
        function_root_variable.name = "<locals>".to_string();
        function_root_variable = self.variable_cache.cache_variable(
            stackframe_root_variable.variable_key,
            function_root_variable,
            core,
        )?;

        unit_info.process_tree(
            function_node,
            function_root_variable,
            core,
            stack_frame_registers,
        )?;
        Ok(())
    }

    /// This is a lazy/deffered resolves and loads all the 'child' `Variable`s for a given unit.
    /// This is used for:
    /// - pointer variables (`DW_TAG_pointer_type`) into the `DebugInfo::VariableCache`.
    /// - <statics> : The load of static variables and their namespaces in the debugger.
    pub fn cache_referenced_variables(
        &self,
        core: &mut Core<'_>,
        parent_variable: Variable,
    ) -> Result<(), DebugError> {
        // Only do attempt this part if the parent is a pointer and we have not yet resolved the referenced children.
        if parent_variable.referenced_node_offset.is_some()
            && !self.variable_cache.has_children(&parent_variable)?
        {
            if let Some(ref stack_frame_registers) = parent_variable.stack_frame_registers {
                if let Some(header_offset) = parent_variable.header_offset {
                    let unit_header = self.dwarf.debug_info.header_from_offset(header_offset)?;
                    let unit_info = UnitInfo {
                        debug_info: self,
                        unit: gimli::Unit::new(&self.dwarf, unit_header)?,
                    };
                    // Reference to a type, or an node.entry() to another type or a type modifier which will point to another type.
                    let mut type_tree = unit_info.unit.header.entries_tree(
                        &unit_info.unit.abbreviations,
                        parent_variable.referenced_node_offset,
                    )?;
                    let referenced_node = type_tree.root()?;
                    let mut referenced_variable = self.variable_cache.cache_variable(
                        parent_variable.variable_key,
                        Variable::new(
                            unit_info.unit.header.offset().as_debug_info_offset(),
                            Some(referenced_node.entry().offset()),
                        ),
                        core,
                    )?;
                    if parent_variable.name.starts_with("Some") {
                        referenced_variable.name = parent_variable.name.replacen("&", "*", 1);
                    } else {
                        referenced_variable.name = format!("*{}", parent_variable.name);
                        // Now, retrieve the location by reading the adddress pointed to by the parent variable.
                    }
                    let mut buff = [0u8; 4];
                    core.read(parent_variable.memory_location as u32, &mut buff)?;
                    referenced_variable.memory_location = u32::from_le_bytes(buff) as u64;
                    referenced_variable = self.variable_cache.cache_variable(
                        referenced_variable.parent_key,
                        referenced_variable,
                        core,
                    )?;
                    referenced_variable = unit_info.extract_type(
                        referenced_node,
                        &parent_variable,
                        referenced_variable,
                        core,
                        stack_frame_registers,
                    )?;

                    // Only use this, if it is NOT a unit datatype.
                    if referenced_variable.type_name.eq("()") {
                        self.variable_cache
                            .remove_cache_entry(referenced_variable.variable_key)?;
                    } else if parent_variable.name.eq("<statics>") {
                        // If we are lazily resolving `<statics>`, then we need to eliminate the intermediate node
                        self.variable_cache
                            .adopt_grand_children(&parent_variable, &referenced_variable)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns a populated (resolved) [`StackFrame`] struct.
    /// This function will also populate the `DebugInfo::VariableCache` with in scope `Variable`s for each `StackFrame`
    fn get_stackframe_info(
        &self,
        core: &mut Core<'_>,
        address: u64,
        unwind_registers: &Registers,
    ) -> Result<StackFrame, DebugError> {
        let mut units = self.get_units();

        let unknown_function = format!("<unknown function @ {:#010x}>", address);
        let mut stack_frame_registers = unwind_registers.clone();

        while let Some(unit_info) = self.get_next_unit_info(&mut units) {
            if let Some(function_die) = &mut unit_info.get_function_die(address, true) {
                let function_name = function_die
                    .function_name(&unit_info)
                    .unwrap_or(unknown_function);

                if function_die.is_inline {
                    // Calculate the call site for this function, so that we can use it later to create an additional 'callee' `StackFrame` from that PC.
                    let address_size =
                        gimli::_UnwindSectionPrivate::address_size(&self.frame_section) as u64;
                    if function_die.low_pc > address_size && function_die.low_pc < u32::MAX.into() {
                        stack_frame_registers
                            .set_return_address(Some((function_die.low_pc - address_size) as u32));
                    } else {
                        stack_frame_registers.set_return_address(None);
                    }
                }
                // To correctly show breakpoints, we use the source location where the function is declared, NOT the source location at the call site of any inlined_functions.
                let function_source_location = self.get_source_location(address);

                log::debug!("UNWIND: Function name: {}", function_name);

                // Now that we have the function_name and function_source_location, we can cache the in-scope `Variable`s (`<statics>` and `<locals>`) in `DebugInfo::VariableCache`
                // We need an empty parent variable for the next operation, but do not need to store it in the cache.
                let parent_variable = Variable::new(None, None);
                let mut stackframe_root_variable = Variable::new(
                    unit_info.unit.header.offset().as_debug_info_offset(),
                    Some(function_die.function_die.offset()),
                );
                stackframe_root_variable.name = "<stack_frame>".to_string();
                stackframe_root_variable.source_location = function_source_location.clone();
                stackframe_root_variable.set_value(function_name.clone());
                stackframe_root_variable = unit_info.extract_location(
                    &unit_info
                        .unit
                        .entries_tree(Some(function_die.function_die.offset()))?
                        .root()?,
                    &parent_variable,
                    stackframe_root_variable,
                    core,
                    &stack_frame_registers,
                )?;

                stackframe_root_variable = self.variable_cache.cache_variable(
                    parent_variable.variable_key,
                    stackframe_root_variable,
                    core,
                )?;

                if let Some(error) = self
                    .cache_register_variables(
                        &stack_frame_registers,
                        &stackframe_root_variable,
                        core,
                    )
                    .err()
                {
                    log::warn!(
                        "Could not resolve register variables.{}\nContinuing.",
                        error
                    );
                }

                // Next, resolve the statics that belong to the compilation unit that this function is in.
                if let Some(error) = self
                    .cache_static_variables(
                        core,
                        &unit_info,
                        &stack_frame_registers,
                        &stackframe_root_variable,
                    )
                    .err()
                {
                    log::warn!(
                        "Error while resolving static variables.{}\nContinuing.",
                        error
                    );
                }

                // Next, resolve and cache the function variables.
                self.cache_function_variables(
                    core,
                    function_die,
                    &unit_info,
                    &stack_frame_registers,
                    &stackframe_root_variable,
                )?;

                // Ready to go ...
                return Ok(StackFrame {
                    // MS DAP Specification requires the id to be unique accross all threads, so using  so using unique `Variable::variable_key` of the `stackframe_root_variable` as the id.
                    id: stackframe_root_variable.variable_key as u64,
                    function_name,
                    source_location: function_source_location,
                    registers: stack_frame_registers,
                    pc: address as u32,
                });
            }
        }

        // If we get here, we were not able to identify/unwind the function information
        Ok(StackFrame {
            id: u64::MAX, // This value is outside the i64 range used by MS DAP Protocol, and will be used to identify that this is not a "real" StackFrame
            function_name: unknown_function,
            source_location: self.get_source_location(address),
            registers: stack_frame_registers,
            pc: address as u32,
        })
    }

    pub fn try_unwind<'probe, 'core>(
        &mut self,
        core: &'core mut Core<'probe>,
        address: u64,
    ) -> StackFrameIterator<'_, 'probe, 'core> {
        self.variable_cache = VariableCache::new();
        StackFrameIterator::new(self, core, address)
    }

    /// Find the program counter where a breakpoint should be set,
    /// given a source file, a line and optionally a column.
    pub fn get_breakpoint_location(
        &self,
        path: &Path,
        line: u64,
        column: Option<u64>,
    ) -> Result<Option<u64>, DebugError> {
        log::debug!(
            "Looking for breakpoint location for {}:{}:{}",
            path.display(),
            line,
            column
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_owned())
        );

        let mut unit_iter = self.dwarf.units();

        let mut locations = Vec::new();

        while let Some(unit_header) = unit_iter.next()? {
            let unit = self.dwarf.unit(unit_header)?;

            if let Some(ref line_program) = unit.line_program {
                let header = line_program.header();

                for file_name in header.file_names() {
                    let combined_path = self.get_path(&unit, header, file_name);

                    if combined_path.map(|p| p == path).unwrap_or(false) {
                        let mut rows = line_program.clone().rows();

                        while let Some((header, row)) = rows.next_row()? {
                            let row_path = row
                                .file(header)
                                .and_then(|file_entry| self.get_path(&unit, header, file_entry));

                            if row_path.map(|p| p != path).unwrap_or(true) {
                                continue;
                            }

                            if let Some(cur_line) = row.line() {
                                if cur_line.get() == line {
                                    locations.push((row.address(), row.column()));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Look for the break point location for the best match based on the column specified.
        match locations.len() {
            0 => Ok(None),
            1 => Ok(Some(locations[0].0)),
            n => {
                log::debug!("Found {} possible breakpoint locations", n);

                locations.sort_by({
                    |a, b| {
                        if a.1 != b.1 {
                            a.1.cmp(&b.1)
                        } else {
                            a.0.cmp(&b.0)
                        }
                    }
                });

                for loc in &locations {
                    log::debug!("col={:?}, addr={:#010x}", loc.1, loc.0);
                }

                match column {
                    Some(search_col) => {
                        let mut best_location = &locations[0];

                        let search_col = match NonZeroU64::new(search_col) {
                            None => gimli::read::ColumnType::LeftEdge,
                            Some(c) => gimli::read::ColumnType::Column(c),
                        };

                        for loc in &locations[1..] {
                            if loc.1 > search_col {
                                break;
                            }

                            if best_location.1 < loc.1 {
                                best_location = loc;
                            }
                        }

                        Ok(Some(best_location.0))
                    }
                    None => Ok(Some(locations[0].0)),
                }
            }
        }
    }

    /// Get the absolute path for an entry in a line program header
    fn get_path(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        header: &LineProgramHeader<DwarfReader>,
        file_entry: &FileEntry<DwarfReader>,
    ) -> Option<PathBuf> {
        let file_name_attr_string = self.dwarf.attr_string(unit, file_entry.path_name()).ok()?;
        let dir_name_attr_string = file_entry
            .directory(header)
            .and_then(|dir| self.dwarf.attr_string(unit, dir).ok());

        let name_path = Path::new(from_utf8(&file_name_attr_string).ok()?);

        let dir_path =
            dir_name_attr_string.and_then(|dir_name| from_utf8(&dir_name).ok().map(PathBuf::from));

        let mut combined_path = match dir_path {
            Some(dir_path) => dir_path.join(name_path),
            None => name_path.to_owned(),
        };

        if combined_path.is_relative() {
            let comp_dir = unit
                .comp_dir
                .as_ref()
                .map(|dir| from_utf8(dir))
                .transpose()
                .ok()?
                .map(PathBuf::from);

            if let Some(comp_dir) = comp_dir {
                combined_path = comp_dir.join(&combined_path);
            }
        }

        Some(combined_path)
    }

    fn find_file_and_directory(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        header: &LineProgramHeader<DwarfReader>,
        file_entry: &FileEntry<DwarfReader>,
    ) -> Option<(Option<String>, Option<PathBuf>)> {
        let combined_path = self.get_path(unit, header, file_entry)?;

        let file_name = combined_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());

        let directory = combined_path.parent().map(|p| p.to_path_buf());

        Some((file_name, directory))
    }
}

/// Reference to a DIE for a function
struct FunctionDie<'abbrev, 'unit> {
    function_die: FunctionDieType<'abbrev, 'unit>,
    is_inline: bool,
    abstract_die: Option<FunctionDieType<'abbrev, 'unit>>,
    low_pc: u64,
    high_pc: u64,
}

// TODO: We should consider replacing the `panic`s with proper error handling, that allows a user to be 'partially' successful with a debug session. If we use `panic`, then the user will have to wait until the bug is fixed before they can continue trying to use probe-rs
impl<'debugunit, 'abbrev, 'unit: 'debugunit> FunctionDie<'abbrev, 'unit> {
    fn new(die: FunctionDieType<'abbrev, 'unit>) -> Self {
        let tag = die.tag();

        match tag {
            gimli::DW_TAG_subprogram => {
                Self {
                    function_die: die,
                    is_inline: false,
                    abstract_die: None,
                    low_pc: 0,
                    high_pc: 0,
                }
            }
            other_tag => panic!("FunctionDie has to has to have Tag DW_TAG_subprogram, but tag is {:?}. This is a bug, please report it.", other_tag.static_string())
        }
    }

    fn new_inlined(
        concrete_die: FunctionDieType<'abbrev, 'unit>,
        abstract_die: FunctionDieType<'abbrev, 'unit>,
    ) -> Self {
        let tag = concrete_die.tag();

        match tag {
            gimli::DW_TAG_inlined_subroutine => {
                Self {
                    function_die: concrete_die,
                    is_inline: true,
                    abstract_die: Some(abstract_die),
                    low_pc: 0,
                    high_pc: 0,

                }
            }
            other_tag => panic!("FunctionDie has to has to have Tag DW_TAG_inlined_subroutine, but tag is {:?}. This is a bug, please report it.", other_tag.static_string())
        }
    }

    fn function_name(&self, unit: &UnitInfo<'_>) -> Option<String> {
        if let Some(fn_name_attr) = self.get_attribute(gimli::DW_AT_name) {
            match fn_name_attr.value() {
                gimli::AttributeValue::DebugStrRef(fn_name_ref) => {
                    let fn_name_raw = unit.debug_info.dwarf.string(fn_name_ref).unwrap();

                    Some(String::from_utf8_lossy(&fn_name_raw).to_string())
                }
                value => {
                    log::debug!("Unexpected attribute value for DW_AT_name: {:?}", value);
                    None
                }
            }
        } else {
            log::debug!("DW_AT_name attribute not found, unable to retrieve function name");
            None
        }
    }

    fn get_attribute(&self, attribute_name: gimli::DwAt) -> Option<GimliAttribute> {
        let attribute = self
            .function_die
            .attr(attribute_name)
            .expect(" Failed to parse entry");

        // For inlined function, the *abstract instance* has to be checked if we cannot find the
        // attribute on the *concrete instance*.
        if self.is_inline && attribute.is_none() {
            let origin = self.abstract_die.as_ref().unwrap();

            origin.attr(attribute_name).expect("Failed to parse entry")
        } else {
            attribute
        }
    }
}

struct UnitInfo<'debuginfo> {
    debug_info: &'debuginfo DebugInfo,
    unit: gimli::Unit<GimliReader, usize>,
}

impl<'debuginfo> UnitInfo<'debuginfo> {
    /// Get the DIE for the function containing the given address.
    fn get_function_die(&self, address: u64, find_inlined: bool) -> Option<FunctionDie> {
        log::trace!("Searching Function DIE for address {:#010x}", address);

        let mut entries_cursor = self.unit.entries();

        while let Ok(Some((_depth, current))) = entries_cursor.next_dfs() {
            if current.tag() == gimli::DW_TAG_subprogram {
                let mut ranges = self
                    .debug_info
                    .dwarf
                    .die_ranges(&self.unit, current)
                    .unwrap();

                while let Ok(Some(ranges)) = ranges.next() {
                    if (ranges.begin <= address) && (address < ranges.end) {
                        // Check if we are actually in an inlined function

                        let mut die = FunctionDie::new(current.clone());
                        die.low_pc = ranges.begin;
                        die.high_pc = ranges.end;
                        if find_inlined {
                            log::debug!(
                                "Found DIE, now checking for inlined functions: name={:?}",
                                die.function_name(self)
                            );
                            return self
                                .find_inlined_function(address, current.offset())
                                .or_else(|| {
                                    log::debug!("No inlined function found!");
                                    Some(die)
                                });
                        } else {
                            log::debug!("Found DIE: name={:?}", die.function_name(self));

                            return Some(die);
                        }
                    }
                }
            }
        }
        None
    }

    /// Check if the function located at the given offset contains an inlined function at the
    /// given address.
    fn find_inlined_function(&self, address: u64, offset: UnitOffset) -> Option<FunctionDie> {
        let mut current_depth = 0;

        let mut cursor = self.unit.entries_at_offset(offset).unwrap();

        while let Ok(Some((depth, current))) = cursor.next_dfs() {
            current_depth += depth;

            if current_depth < 0 {
                break;
            }

            if current.tag() == gimli::DW_TAG_inlined_subroutine {
                let mut ranges = self
                    .debug_info
                    .dwarf
                    .die_ranges(&self.unit, current)
                    .unwrap();

                while let Ok(Some(ranges)) = ranges.next() {
                    if (ranges.begin <= address) && (address < ranges.end) {
                        // Check if we are actually in an inlined function

                        // Find the abstract definition
                        if let Some(abstract_origin) =
                            current.attr(gimli::DW_AT_abstract_origin).unwrap()
                        {
                            match abstract_origin.value() {
                                gimli::AttributeValue::UnitRef(unit_ref) => {
                                    let abstract_die = self.unit.entry(unit_ref).unwrap();
                                    let mut die = FunctionDie::new_inlined(
                                        current.clone(),
                                        abstract_die.clone(),
                                    );
                                    die.low_pc = ranges.begin;
                                    die.high_pc = ranges.end;
                                    return Some(die);
                                }
                                other_value => panic!("Unsupported value: {:?}", other_value),
                            }
                        } else {
                            return None;
                        }
                    }
                }
            }
        }

        None
    }

    fn expr_to_piece(
        &self,
        core: &mut Core<'_>,
        expression: gimli::Expression<GimliReader>,
        stack_frame_registers: &Registers,
    ) -> Result<Vec<gimli::Piece<GimliReader, usize>>, DebugError> {
        let mut evaluation = expression.evaluation(self.unit.encoding());
        let frame_base = if let Some(frame_base) = stack_frame_registers.get_frame_pointer() {
            u64::from(frame_base)
        } else {
            return Err(DebugError::Other(anyhow::anyhow!(
                "Cannot unwind `Variable` location without a valid CFA (canonical frame address)"
            )));
        };
        // go for evaluation
        let mut result = evaluation.evaluate()?;

        loop {
            use gimli::EvaluationResult::*;

            result = match result {
                Complete => break,
                RequiresMemory { address, size, .. } => {
                    let mut buff = vec![0u8; size as usize];
                    core.read(address as u32, &mut buff).map_err(|_| {
                        DebugError::Other(anyhow::anyhow!("Unexpected error while reading debug expressions from target memory. Please report this as a bug."))
                    })?;
                    match size {
                        1 => evaluation.resume_with_memory(gimli::Value::U8(buff[0]))?,
                        2 => {
                            let val = (u16::from(buff[0]) << 8) | (u16::from(buff[1]) as u16);
                            evaluation.resume_with_memory(gimli::Value::U16(val))?
                        }
                        4 => {
                            let val = (u32::from(buff[0]) << 24)
                                | (u32::from(buff[1]) << 16)
                                | (u32::from(buff[2]) << 8)
                                | u32::from(buff[3]);
                            evaluation.resume_with_memory(gimli::Value::U32(val))?
                        }
                        x => {
                            todo!(
                                "Requested memory with size {}, which is not supported yet.",
                                x
                            );
                        }
                    }
                }
                RequiresFrameBase => match evaluation.resume_with_frame_base(frame_base) {
                    Ok(evaluation_result) => evaluation_result,
                    Err(error) => {
                        return Err(DebugError::Other(anyhow::anyhow!(
                            "Error while calculating `Variable::memory_location`:{}.",
                            error
                        )))
                    }
                },
                RequiresRegister {
                    register,
                    base_type,
                } => {
                    let raw_value = stack_frame_registers
                        .get_value_by_dwarf_register_number(register.0 as u32)
                        .expect("Failed to read register from `StackFrame::registers`");

                    if base_type != gimli::UnitOffset(0) {
                        todo!(
                            "Support for units in RequiresRegister request is not yet implemented."
                        )
                    }

                    evaluation.resume_with_register(gimli::Value::Generic(raw_value as u64))?
                }
                RequiresRelocatedAddress(address_index) => {
                    if address_index.is_zero() {
                        // This is a rust-lang bug for statics ... https://github.com/rust-lang/rust/issues/32574.
                        evaluation.resume_with_relocated_address(u64::MAX)?
                    } else {
                        // The address_index as an offset from 0, so just pass it into the next step.
                        evaluation.resume_with_relocated_address(address_index)?
                    }
                }
                x => {
                    todo!("expr_to_piece {:?}", x)
                }
            }
        }
        Ok(evaluation.result())
    }

    /// Recurse the ELF structure below the `tree_node`, and ...
    /// - Consumes the `child_variable`.
    /// - Updates the `DebugInfo::VariableCache` with all appropriate `Variable` fields.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    fn process_tree_node_attributes(
        &self,
        tree_node: &mut gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &mut Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &Registers,
    ) -> Result<Variable, DebugError> {
        // Identify the parent.
        child_variable.parent_key = parent_variable.variable_key;

        // It often happens that intermediate nodes exist for structure reasons,
        // so we need to pass values like 'member_index' from the parent down to the next level child nodes.
        if parent_variable.member_index.is_some() {
            child_variable.member_index = parent_variable.member_index;
        }

        // For variable attribute resolution, we need to resolve a few attributes in advance of looping through all the other ones.

        // We need to process the location attribute to ensure that location is known before we calculate type.
        child_variable = self.extract_location(
            tree_node,
            parent_variable,
            child_variable,
            core,
            stack_frame_registers,
        )?;

        // We need to determine if we are working with a 'abstract` location, and use that node for the attributes we need
        // let mut origin_tree:Option<gimli::EntriesTree<GimliReader<>>> = None;
        let attributes_entry = if let Ok(Some(abstract_origin)) =
            tree_node.entry().attr(gimli::DW_AT_abstract_origin)
        {
            match abstract_origin.value() {
                gimli::AttributeValue::UnitRef(unit_ref) => Some(
                    self.unit
                        .header
                        .entries_tree(&self.unit.abbreviations, Some(unit_ref))?
                        .root()?
                        .entry()
                        .clone(),
                ),
                other_attribute_value => {
                    child_variable.set_value(format!(
                        "UNIMPLEMENTED: Attribute Value for DW_AT_abstract_origin {:?}",
                        other_attribute_value
                    ));
                    None
                }
            }
        } else {
            Some(tree_node.entry().clone())
        };

        if let Some(attributes_entry) = attributes_entry {
            let mut variable_attributes = attributes_entry.attrs();

            // Now loop through all the unit attributes to extract the remainder of the `Variable` definition.
            while let Ok(Some(attr)) = variable_attributes.next() {
                match attr.name() {
                    gimli::DW_AT_location | gimli::DW_AT_data_member_location => {
                        // The child_variable.location is calculated with attribute gimli::DW_AT_type, to ensure it gets done before DW_AT_type is processed
                    }
                    gimli::DW_AT_name => {
                        child_variable.name = extract_name(self.debug_info, attr.value());
                    }
                    gimli::DW_AT_decl_file => {
                        if let Some((directory, file_name)) =
                            extract_file(self.debug_info, &self.unit, attr.value())
                        {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: existing_source_location.line,
                                        column: existing_source_location.column,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: None,
                                        column: None,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                    });
                                }
                            }
                        };
                    }
                    gimli::DW_AT_decl_line => {
                        if let Some(line_number) = extract_line(self.debug_info, attr.value()) {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: existing_source_location.column,
                                        file: existing_source_location.file,
                                        directory: existing_source_location.directory,
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: None,
                                        file: None,
                                        directory: None,
                                    });
                                }
                            }
                        };
                    }
                    gimli::DW_AT_decl_column => {
                        // Unused.
                    }
                    gimli::DW_AT_containing_type => {
                        // TODO: Implement [documented RUST extensions to DWARF standard](https://rustc-dev-guide.rust-lang.org/debugging-support-in-rustc.html?highlight=dwarf#dwarf-and-rustc)
                    }
                    gimli::DW_AT_type => {
                        match attr.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                // Reference to a type, or an entry to another type or a type modifier which will point to another type.
                                let mut type_tree = self
                                    .unit
                                    .header
                                    .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                                let tree_node = type_tree.root()?;
                                child_variable = self.extract_type(
                                    tree_node,
                                    parent_variable,
                                    child_variable,
                                    core,
                                    stack_frame_registers,
                                )?;
                            }
                            other_attribute_value => {
                                child_variable.set_value(format!(
                                    "UNIMPLEMENTED: Attribute Value for DW_AT_type {:?}",
                                    other_attribute_value
                                ));
                            }
                        }
                    }
                    gimli::DW_AT_enum_class => match attr.value() {
                        gimli::AttributeValue::Flag(is_enum_class) => {
                            if is_enum_class {
                                child_variable.set_value(child_variable.type_name.clone());
                            } else {
                                child_variable.set_value(format!(
                                    "UNIMPLEMENTED: Flag Value for DW_AT_enum_class {:?}",
                                    is_enum_class
                                ));
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: Attribute Value for DW_AT_enum_class: {:?}",
                                other_attribute_value
                            ));
                        }
                    },
                    gimli::DW_AT_const_value => match attr.value() {
                        gimli::AttributeValue::Udata(const_value) => {
                            child_variable.set_value(const_value.to_string());
                        }
                        other_attribute_value => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: Attribute Value for DW_AT_const_value: {:?}",
                                other_attribute_value
                            ));
                        }
                    },
                    gimli::DW_AT_alignment => {
                        // TODO: Figure out when (if at all) we need to do anything with DW_AT_alignment for the purposes of decoding data values.
                    }
                    gimli::DW_AT_artificial => {
                        // These are references for entries like discriminant values of `VariantParts`.
                        child_variable.name = "<artificial>".to_string();
                    }
                    gimli::DW_AT_discr => match attr.value() {
                        // This calculates the active discriminant value for the `VariantPart`.
                        gimli::AttributeValue::UnitRef(unit_ref) => {
                            let mut type_tree = self
                                .unit
                                .header
                                .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                            let mut discriminant_node = type_tree.root()?;
                            let mut discriminant_variable =
                                self.debug_info.variable_cache.cache_variable(
                                    parent_variable.variable_key,
                                    Variable::new(
                                        self.unit.header.offset().as_debug_info_offset(),
                                        Some(discriminant_node.entry().offset()),
                                    ),
                                    core,
                                )?;
                            discriminant_variable = self.process_tree_node_attributes(
                                &mut discriminant_node,
                                parent_variable,
                                discriminant_variable,
                                core,
                                stack_frame_registers,
                            )?;
                            parent_variable.role = VariantRole::VariantPart(
                                discriminant_variable
                                    .get_value(&self.debug_info.variable_cache)
                                    .parse()
                                    .unwrap_or(u64::MAX) as u64,
                            );
                            self.debug_info
                                .variable_cache
                                .remove_cache_entry(discriminant_variable.variable_key)?;
                        }
                        other_attribute_value => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: Attribute Value for DW_AT_discr {:?}",
                                other_attribute_value
                            ));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_lower_bound => match attr.value().udata_value() {
                        Some(lower_bound) => child_variable.range_lower_bound = lower_bound as i64,
                        None => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: Attribute Value for DW_AT_lower_bound: {:?}",
                                attr.value()
                            ));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_upper_bound | gimli::DW_AT_count => match attr
                        .value()
                        .udata_value()
                    {
                        Some(upper_bound) => child_variable.range_upper_bound = upper_bound as i64,
                        None => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: Attribute Value for DW_AT_upper_bound: {:?}",
                                attr.value()
                            ));
                        }
                    },
                    gimli::DW_AT_external => {
                        // TODO: Implement globally visible variables.
                    }
                    gimli::DW_AT_declaration => {
                        // Unimplemented.
                    }
                    gimli::DW_AT_encoding => {
                        // Ignore these. RUST data types handle this intrinsicly.
                    }
                    gimli::DW_AT_discr_value => {
                        // Processed by `extract_variant_discriminant()`.
                    }
                    gimli::DW_AT_byte_size => {
                        // Processed by `extract_byte_size()`.
                    }
                    gimli::DW_AT_abstract_origin => {
                        // Processed before looping through all attributes
                    }
                    gimli::DW_AT_linkage_name => {
                        // Unused attribute of, for example, inlined DW_TAG_subroutine
                    }
                    gimli::DW_AT_address_class => {
                        // Processed by `extract_type()`
                    }
                    other_attribute => {
                        child_variable.set_value(format!(
                            "UNIMPLEMENTED: Variable Attribute {:?} : {:?}, with children = {}",
                            other_attribute.static_string(),
                            tree_node
                                .entry()
                                .attr_value(other_attribute)
                                .unwrap()
                                .unwrap(),
                            tree_node.entry().has_children()
                        ));
                    }
                }
            }
        }
        self.debug_info
            .variable_cache
            .cache_variable(child_variable.parent_key, child_variable, core)
            .map_err(|error| error.into())
    }

    /// Recurse the ELF structure below the `parent_node`, and ...
    /// - Consumes the `parent_variable`.
    /// - Updates the `DebugInfo::VariableCache` with all descendant `Variable`s.
    /// - Returns a clone of the most up-to-date `parent_variable` in the cache.
    fn process_tree(
        &self,
        parent_node: gimli::EntriesTreeNode<GimliReader>,
        mut parent_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &Registers,
    ) -> Result<Variable, DebugError> {
        let program_counter =
            if let Some(program_counter) = stack_frame_registers.get_program_counter() {
                u64::from(program_counter)
            } else {
                return Err(DebugError::Other(anyhow::anyhow!(
                    "Cannot unwind `Variable` without a valid PC (program_counter)"
                )));
            };
        let mut child_nodes = parent_node.children();
        while let Some(mut child_node) = child_nodes.next()? {
            match child_node.entry().tag() {
                gimli::DW_TAG_namespace => {
                    // Use these parents to extract `statics`.
                    let mut namespace_variable = Variable::new(
                        self.unit.header.offset().as_debug_info_offset(),
                        Some(child_node.entry().offset()),
                );
                    namespace_variable.name = if let Ok(Some(attr)) = child_node.entry().attr(gimli::DW_AT_name) {
                        extract_name(self.debug_info, attr.value())
                    } else {"<anonymous namespace>".to_string()};
                    namespace_variable.type_name = "<namespace>".to_string();
                    namespace_variable.memory_location = 0;
                    namespace_variable = self.debug_info.variable_cache.cache_variable(parent_variable.variable_key, namespace_variable, core)?;
                    let mut namespace_children_nodes = child_node.children();
                    while let Some(mut namespace_child_node) = namespace_children_nodes.next()? {
                        match namespace_child_node.entry().tag() {
                            gimli::DW_TAG_variable => {
                                // We only want the TOP level variables of the namespace (statics).
                                let static_child_variable = self.debug_info.variable_cache.cache_variable(namespace_variable.variable_key, Variable::new(
                                    self.unit.header.offset().as_debug_info_offset(),
                                    Some(namespace_child_node.entry().offset()),), core)?;
                                self.process_tree_node_attributes(&mut namespace_child_node, &mut namespace_variable, static_child_variable, core, stack_frame_registers)?;
                            }
                            gimli::DW_TAG_namespace => {
                                // Recurse for additional namespace variables.
                                let mut namespace_child_variable = Variable::new(
                                    self.unit.header.offset().as_debug_info_offset(),
                                    Some(namespace_child_node.entry().offset()),);
                                namespace_child_variable.name = if let Ok(Some(attr)) = namespace_child_node.entry().attr(gimli::DW_AT_name) {
                                    format!("{}::{}", namespace_variable.name, extract_name(self.debug_info, attr.value()))
                                } else {"<anonymous namespace>".to_string()};
                                namespace_child_variable.type_name = "<namespace>".to_string();
                                namespace_child_variable.memory_location = 0;
                                namespace_child_variable = self.debug_info.variable_cache.cache_variable(namespace_variable.variable_key, namespace_child_variable, core)?;
                                namespace_child_variable = self.process_tree(namespace_child_node, namespace_child_variable, core, stack_frame_registers)?;
                                if !self.debug_info.variable_cache.has_children(&namespace_child_variable)? {
                                    self.debug_info.variable_cache.remove_cache_entry(namespace_child_variable.variable_key)?;
                                }
                            }
                            _ => {
                                // We only want namespace and variable children.
                            }
                        }
                    }
                    if !self.debug_info.variable_cache.has_children(&namespace_variable)? {
                        self.debug_info.variable_cache.remove_cache_entry(namespace_variable.variable_key)?;
                    }
                }
                gimli::DW_TAG_variable |    // Typical top-level variables.
                gimli::DW_TAG_member |      // Members of structured types.
                gimli::DW_TAG_enumerator    // Possible values for enumerators, used by extract_type() when processing DW_TAG_enumeration_type.
                => {
                    let mut child_variable = self.debug_info.variable_cache.cache_variable(parent_variable.variable_key, Variable::new(
                    self.unit.header.offset().as_debug_info_offset(),
                    Some(child_node.entry().offset()),
                ), core)?;
                    child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers)?;
                    // Do not keep or process PhantomData nodes, or variant parts that we have already used.
                    if child_variable.type_name.starts_with("PhantomData") 
                        ||  child_variable.name == "<artificial>"
                    {
                        self.debug_info.variable_cache.remove_cache_entry(child_variable.variable_key)?;
                    } else if child_variable.type_name == "Some" {
                        //This is an intermediate node. Once we've resolved the children, we can adopt them to their grandparent
                        self.debug_info.variable_cache.adopt_grand_children(&parent_variable, &child_variable)?;
                    }
                    else {
                        // Recursively process each child.
                        self.process_tree(child_node, child_variable, core, stack_frame_registers)?;
                    }
                }
                gimli::DW_TAG_variant_part => {
                    // We need to recurse through the children, to find the DW_TAG_variant with discriminant matching the DW_TAG_variant, 
                    // and ONLY add it's children to the parent variable. 
                    // The structure looks like this (there are other nodes in the structure that we use and discard before we get here):
                    // Level 1: --> An actual variable that has a variant value
                    //      Level 2: --> this DW_TAG_variant_part node (some child nodes are used to calc the active Variant discriminant)
                    //          Level 3: --> Some DW_TAG_variant's that have discriminant values to be matched against the discriminant 
                    //              Level 4: --> The actual variables, with matching discriminant, which will be added to `parent_variable`
                    // TODO: Handle Level 3 nodes that belong to a DW_AT_discr_list, instead of having a discreet DW_AT_discr_value 
                    let mut child_variable = self.debug_info.variable_cache.cache_variable(
                        parent_variable.variable_key,
                        Variable::new(self.unit.header.offset().as_debug_info_offset(),Some(child_node.entry().offset())),
                        core
                    )?;
                    // To determine the discriminant, we use the following rules:
                    // - If there is no DW_AT_discr, then there will be a single DW_TAG_variant, and this will be the matching value. In the code here, we assign a default value of u64::MAX to both, so that they will be matched as belonging together (https://dwarfstd.org/ShowIssue.php?issue=180517.2)
                    // - TODO: The [DWARF] standard, 5.7.10, allows for a case where there is no DW_AT_discr attribute, but a DW_AT_type to represent the tag. I have not seen that generated from RUST yet.
                    // - If there is a DW_AT_discr that has a value, then this is a reference to the member entry for the discriminant. This value will be resolved to match against the appropriate DW_TAG_variant.
                    // - TODO: The [DWARF] standard, 5.7.10, allows for a DW_AT_discr_list, but I have not seen that generated from RUST yet. 
                    parent_variable.role = VariantRole::VariantPart(u64::MAX);
                    child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers)?;
                    // At this point we have everything we need (It has updated the parent's `role`) from the child_variable, so elimnate it before we continue ...
                    self.debug_info.variable_cache.remove_cache_entry(child_variable.variable_key)?;
                    parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers)?;
                }
                gimli::DW_TAG_variant // variant is a child of a structure, and one of them should have a discriminant value to match the DW_TAG_variant_part 
                => {
                    // We only need to do this if we have not already found our variant,
                    if !self.debug_info.variable_cache.has_children(&parent_variable)? {
                        let mut child_variable = self.debug_info.variable_cache.cache_variable(
                            parent_variable.variable_key,
                            Variable::new(self.unit.header.offset().as_debug_info_offset(), Some(child_node.entry().offset())),
                            core
                        )?;
                        self.extract_variant_discriminant(&child_node, &mut child_variable)?;
                        child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, core, stack_frame_registers)?;
                        if let VariantRole::Variant(discriminant) = child_variable.role {
                            // Only process the discriminant variants or when we eventually   encounter the default 
                            if parent_variable.role == VariantRole::VariantPart(discriminant) || discriminant == u64::MAX
                            {
                                // Pass some key values through intermediate nodes to valid desccendants.
                                child_variable.memory_location = parent_variable.memory_location;
                                // Recursively process each relevant child node.
                                child_variable = self.process_tree(child_node, child_variable, core, stack_frame_registers)?;
                                // Eliminate intermediate DWARF nodes, but keep their children
                                self.debug_info.variable_cache.adopt_grand_children(&parent_variable, &child_variable)?;

                            } else {
                                self.debug_info.variable_cache.remove_cache_entry(child_variable.variable_key)?;
                            }
                        }
                    }
                }
                gimli::DW_TAG_subrange_type => {
                    // This tag is a child node fore parent types such as (array, vector, etc.).
                    // Recursively process each node, but pass the parent_variable so that new children are caught despite missing these tags.
                    let mut range_variable = self.debug_info.variable_cache.cache_variable(parent_variable.variable_key,Variable::new(
                    self.unit.header.offset().as_debug_info_offset(),
                    Some(child_node.entry().offset()),
                ), core)?;
                    range_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, range_variable, core, stack_frame_registers)?;
                    // Pass the pertinent info up to the parent_variable.
                    parent_variable.type_name = range_variable.type_name;
                    parent_variable.range_lower_bound = range_variable.range_lower_bound;
                    parent_variable.range_upper_bound = range_variable.range_upper_bound;
                    self.debug_info.variable_cache.remove_cache_entry(range_variable.variable_key)?;
                }
                gimli::DW_TAG_template_type_parameter => {
                    // The parent node for Rust generic type parameter
                    // These show up as a child of structures they belong to and points to the type that matches the template.
                    // They are followed by a sibling of `DW_TAG_member` with name '__0' that has all the attributes needed to resolve the value.
                    // TODO: If there are multiple types supported, then I suspect there will be additional `DW_TAG_member` siblings. We will need to match those correctly.
                }
                gimli::DW_TAG_formal_parameter => {
                    // TODO: WIP Parameters for functions, closures and inlined functions.
                    // Recursively process each child.
                    parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers)?;
                }
                gimli::DW_TAG_inlined_subroutine => {
                    // Recurse the variables of inlined subroutines as normal, but beware that their name, type, etc. has to be resolved from DW_AT_abstract_origin nodes, and their location has to be passed from here (concrete location) to there (abstract location). 
                    parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers)?;
                }
                gimli::DW_TAG_lexical_block => {
                    // Determine the low and high ranges for which this DIE and children are in scope. These can be specified discreetly, or in ranges. 
                    let mut in_scope =  false;
                    if let Ok(Some(low_pc_attr)) = child_node.entry().attr(gimli::DW_AT_low_pc) {
                        let low_pc = match low_pc_attr.value() {
                            gimli::AttributeValue::Addr(value) => value as u64,
                            _other => u64::MAX,
                        };
                        let high_pc = if let Ok(Some(high_pc_attr))
                            = child_node.entry().attr(gimli::DW_AT_high_pc) {
                                match high_pc_attr.value() {
                                    gimli::AttributeValue::Addr(addr) => addr,
                                    gimli::AttributeValue::Udata(unsigned_offset) => low_pc + unsigned_offset,
                                    _other => 0_u64,
                                }
                        } else { 0_u64};
                        if low_pc == u64::MAX || high_pc == 0_u64 {
                            // These have not been specified correctly ... something went wrong.
                            parent_variable.set_value("ERROR: Processing of variables failed because of invalid/unsupported scope information. Please log a bug at 'https://github.com/probe-rs/probe-rs/issues'".to_string());
                        }
                        if low_pc <= program_counter && program_counter < high_pc {
                            // We have established positive scope, so no need to continue.
                            in_scope = true;
                        };
                        // No scope info yet, so keep looking. 
                    };
                    // Searching for ranges has a bit more overhead, so ONLY do this if do not have scope confirmed yet.
                    if !in_scope {
                        if let Ok(Some(ranges))
                            = child_node.entry().attr(gimli::DW_AT_ranges) {
                                match ranges.value() {
                                    gimli::AttributeValue::RangeListsRef(raw_range_lists_offset) => {
                                        let range_lists_offset = self.debug_info.dwarf.ranges_offset_from_raw(&self.unit, raw_range_lists_offset);

                                        if let Ok(mut ranges) = self
                                            .debug_info
                                            .dwarf
                                            .ranges(&self.unit, range_lists_offset) {
                                                while let Ok(Some(ranges)) = ranges.next() {
                                                    // We have established positive scope, so no need to continue.
                                                    if ranges.begin <= program_counter && program_counter < ranges.end {
                                                        in_scope = true;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    other_range_attribute => {
                                        parent_variable.set_value(format!("Found unexpected scope attribute: {:?} for variable {:?}", other_range_attribute, parent_variable.name));
                                    }
                                }
                        }
                    }
                    if in_scope {
                        // This is IN scope.
                        // Recursively process each child, but pass the parent_variable, so that we don't create intermediate nodes for scope identifiers.
                        parent_variable = self.process_tree(child_node, parent_variable, core, stack_frame_registers)?;
                    }
                }
                other => {
                    // One of two things are true here. Either we've encountered a DwTag that is implemented in `extract_type`, and whould be ignored, or we have encountered an UNIMPLEMENTED  DwTag.
                    match other {
                        gimli::DW_TAG_base_type |
                        gimli::DW_TAG_pointer_type |
                        gimli::DW_TAG_structure_type |
                        gimli::DW_TAG_enumeration_type |
                        gimli::DW_TAG_array_type |
                        gimli::DW_TAG_subroutine_type |
                        gimli::DW_TAG_subprogram |
                        gimli::DW_TAG_union_type => {
                            // These will be processed elsewhere.
                        }
                        unimplemented => {
                            parent_variable.set_value(format!("UNIMPLEMENTED: Encountered unimplemented DwTag {:?} for Variable {:?}", unimplemented.static_string(), parent_variable));
                        }
                    }
                }
            }
        }
        self.debug_info
            .variable_cache
            .cache_variable(parent_variable.parent_key, parent_variable, core)
            .map_err(|error| error.into())
    }

    /// Compute the discriminant value of a DW_TAG_variant variable. If it is not explicitly captured in the DWARF, then it is the default value.
    fn extract_variant_discriminant(
        &self,
        node: &gimli::EntriesTreeNode<GimliReader>,
        variable: &mut Variable,
    ) -> Result<(), DebugError> {
        if node.entry().tag() == gimli::DW_TAG_variant {
            variable.role = match node.entry().attr(gimli::DW_AT_discr_value) {
                Ok(optional_discr_value_attr) => {
                    match optional_discr_value_attr {
                        Some(discr_attr) => {
                            match discr_attr.value() {
                                gimli::AttributeValue::Data1(const_value) => {
                                    VariantRole::Variant(const_value as u64)
                                }
                                other_attribute_value => {
                                    variable.set_value(format!("UNIMPLEMENTED: Attribute Value for DW_AT_discr_value: {:?}", other_attribute_value));
                                    VariantRole::Variant(u64::MAX)
                                }
                            }
                        }
                        None => {
                            // In the case where the variable is a DW_TAG_variant, but has NO DW_AT_discr_value, then this is the "default" to be used.
                            VariantRole::Variant(u64::MAX)
                        }
                    }
                }
                Err(_error) => {
                    variable.set_value(format!(
                        "ERROR: Retrieving DW_AT_discr_value for variable {:?}",
                        variable
                    ));
                    VariantRole::NonVariant
                }
            };
        }
        Ok(())
    }

    /// Compute the type (base to complex) of a variable. Only base types have values.
    /// Complex types are references to node trees, that require traversal in similar ways to other DIE's like functions.
    /// This means both [`get_function_variables()`] and [`extract_type()`] will call the recursive [`process_tree()`] method to build an integrated `tree` of variables with types and values.
    /// - Consumes the `child_variable`.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    fn extract_type(
        &self,
        node: gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &Registers,
    ) -> Result<Variable, DebugError> {
        child_variable.type_name = match node.entry().attr(gimli::DW_AT_name) {
            Ok(optional_name_attr) => match optional_name_attr {
                Some(name_attr) => extract_name(self.debug_info, name_attr.value()),
                None => "<unnamed type>".to_owned(),
            },
            Err(error) => {
                format!("ERROR: evaluating name: {:?} ", error)
            }
        };
        child_variable.byte_size = extract_byte_size(self.debug_info, node.entry());
        match node.entry().tag() {
            gimli::DW_TAG_base_type => {
                if let Some(child_member_index) = child_variable.member_index {
                    // This is a member of an array type, and needs special handling.
                    let (location, has_overflowed) = parent_variable
                        .memory_location
                        .overflowing_add(child_member_index as u64 * child_variable.byte_size);

                    if has_overflowed {
                        return Err(DebugError::Other(anyhow::anyhow!(
                            "Overflow calculating variable address"
                        )));
                    } else {
                        child_variable.memory_location = location;
                    }
                }
            }
            gimli::DW_TAG_pointer_type => {
                // This needs to resolve the pointer before the regular recursion can continue.
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(optional_data_type_attribute) => {
                        match optional_data_type_attribute {
                            Some(data_type_attribute) => {
                                match data_type_attribute.value() {
                                    gimli::AttributeValue::UnitRef(unit_ref) => {
                                        child_variable.referenced_node_offset = Some(unit_ref);
                                        child_variable.stack_frame_registers =
                                            Some(stack_frame_registers.clone());
                                        if child_variable.type_name.starts_with("*const") {
                                            // Resolve the children of this variable, because they contain essential information required to resolve the value
                                            self.debug_info.cache_referenced_variables(
                                                core,
                                                child_variable.clone(),
                                            )?;
                                        } else if parent_variable.type_name == "Some" {
                                            // The parent `DW_TAG_structure_type` with name `Some` is an intermediate node that we only need for its children
                                            // Update the child's name for when we adopt it to the grandparent later on.
                                            child_variable.name =
                                                format!("Some({})", child_variable.type_name);
                                        }
                                        child_variable =
                                            self.debug_info.variable_cache.cache_variable(
                                                parent_variable.variable_key,
                                                child_variable,
                                                core,
                                            )?;
                                    }
                                    other_attribute_value => {
                                        child_variable.set_value(format!(
                                            "UNIMPLEMENTED: Attribute Value for DW_AT_type {:?}",
                                            other_attribute_value
                                        ));
                                    }
                                }
                            }
                            None => {
                                child_variable.set_value(format!(
                                    "ERROR: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        child_variable.set_value(format!(
                            "ERROR: Failed to decode pointer reference: {:?}",
                            error
                        ));
                    }
                }
            }
            gimli::DW_TAG_structure_type => {
                // Recursively process a child types.
                // Unless something is already broken, then don't dig any deeper.
                if child_variable.memory_location != u64::MAX {
                    child_variable =
                        self.process_tree(node, child_variable, core, stack_frame_registers)?;
                }
                if !self
                    .debug_info
                    .variable_cache
                    .has_children(&child_variable)?
                {
                    // Empty structs don't have values. Use the type_name as the display value.
                    child_variable.set_value(child_variable.type_name.clone());
                }
            }
            gimli::DW_TAG_enumeration_type => {
                // Recursively process a child types.
                child_variable =
                    self.process_tree(node, child_variable, core, stack_frame_registers)?;
                let enumerator_values = self
                    .debug_info
                    .variable_cache
                    .get_children(child_variable.variable_key)?;
                // NOTE: hard-coding value of variable.byte_size to 1 ... replace with code if necessary.
                let mut buff = [0u8; 1];
                core.read(child_variable.memory_location as u32, &mut buff)?;
                let this_enum_const_value = u8::from_le_bytes(buff).to_string();
                let enumumerator_value =
                    match enumerator_values.into_iter().find(|enumerator_variable| {
                        enumerator_variable.get_value(&self.debug_info.variable_cache)
                            == this_enum_const_value
                    }) {
                        Some(this_enum) => this_enum.name,
                        None => "<ERROR: Unresolved enum value>".to_string(),
                    };
                child_variable.set_value(format!(
                    "{}::{}",
                    child_variable.type_name, enumumerator_value
                ));
                // We don't need to keep these children.
                self.debug_info
                    .variable_cache
                    .remove_cache_entry_children(child_variable.variable_key)?;
            }
            gimli::DW_TAG_array_type => {
                // This node is a pointer to the type of data stored in the array, with a direct child that contains the range information.
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(optional_data_type_attribute) => {
                        match optional_data_type_attribute {
                            Some(data_type_attribute) => {
                                match data_type_attribute.value() {
                                    gimli::AttributeValue::UnitRef(unit_ref) => {
                                        // First get the DW_TAG_subrange child of this node. It has a DW_AT_type that points to DW_TAG_base_type:__ARRAY_SIZE_TYPE__.
                                        let mut subrange_variable =
                                            self.debug_info.variable_cache.cache_variable(
                                                child_variable.variable_key,
                                                Variable::new(
                                                    self.unit
                                                        .header
                                                        .offset()
                                                        .as_debug_info_offset(),
                                                    Some(node.entry().offset()),
                                                ),
                                                core,
                                            )?;
                                        subrange_variable = self.process_tree(
                                            node,
                                            subrange_variable,
                                            core,
                                            stack_frame_registers,
                                        )?;
                                        child_variable.range_lower_bound =
                                            subrange_variable.range_lower_bound;
                                        child_variable.range_upper_bound =
                                            subrange_variable.range_upper_bound;
                                        if child_variable.range_lower_bound < 0
                                            || child_variable.range_upper_bound < 0
                                        {
                                            child_variable.set_value(format!(
                                                "UNIMPLEMENTED: Array has a sub-range of {}..{} for ",
                                                child_variable.range_lower_bound, child_variable.range_upper_bound)
                                            );
                                        }
                                        self.debug_info
                                            .variable_cache
                                            .remove_cache_entry(subrange_variable.variable_key)?;
                                        // - Next, process this DW_TAG_array_type's DW_AT_type full tree.
                                        // - We have to do this repeatedly, for every array member in the range.
                                        for array_member_index in child_variable.range_lower_bound
                                            ..child_variable.range_upper_bound
                                        {
                                            let mut array_member_type_tree =
                                                self.unit.header.entries_tree(
                                                    &self.unit.abbreviations,
                                                    Some(unit_ref),
                                                )?;
                                            let mut array_member_type_node =
                                                array_member_type_tree.root().unwrap();
                                            let mut array_member_variable =
                                                self.debug_info.variable_cache.cache_variable(
                                                    child_variable.variable_key,
                                                    Variable::new(
                                                        self.unit
                                                            .header
                                                            .offset()
                                                            .as_debug_info_offset(),
                                                        Some(
                                                            array_member_type_node.entry().offset(),
                                                        ),
                                                    ),
                                                    core,
                                                )?;
                                            array_member_variable = self
                                                .process_tree_node_attributes(
                                                    &mut array_member_type_node,
                                                    &mut child_variable,
                                                    array_member_variable,
                                                    core,
                                                    stack_frame_registers,
                                                )?;
                                            child_variable.type_name = format!(
                                                "[{};{}]",
                                                array_member_variable.name,
                                                subrange_variable.range_upper_bound
                                            );
                                            array_member_variable.member_index =
                                                Some(array_member_index);
                                            array_member_variable.name =
                                                format!("__{}", array_member_index);
                                            array_member_variable.source_location =
                                                child_variable.source_location.clone();
                                            self.extract_type(
                                                array_member_type_node,
                                                &child_variable,
                                                array_member_variable,
                                                core,
                                                stack_frame_registers,
                                            )?;
                                        }
                                    }
                                    other_attribute_value => {
                                        child_variable.set_value(format!(
                                            "UNIMPLEMENTED: Attribute Value for DW_AT_type {:?}",
                                            other_attribute_value
                                        ));
                                    }
                                }
                            }
                            None => {
                                child_variable.set_value(format!(
                                    "ERROR: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        child_variable.set_value(format!(
                            "ERROR: Failed to decode pointer reference: {:?}",
                            error
                        ));
                    }
                }
            }
            gimli::DW_TAG_union_type => {
                // Recursively process a child types.
                // TODO: The DWARF does not currently hold information that allows decoding of which UNION arm is instantiated, so we have to display all available.
                child_variable =
                    self.process_tree(node, child_variable, core, stack_frame_registers)?;
                if !self
                    .debug_info
                    .variable_cache
                    .has_children(&child_variable)?
                {
                    // Empty structs don't have values.
                    child_variable.set_value(child_variable.type_name.clone());
                }
            }
            gimli::DW_TAG_subroutine_type => {
                // The type_name will be found in the DW_AT_TYPE child of this entry.
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(optional_data_type_attribute) => match optional_data_type_attribute {
                        Some(data_type_attribute) => match data_type_attribute.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                let subroutine_type_node =
                                    self.unit.header.entry(&self.unit.abbreviations, unit_ref)?;
                                child_variable.type_name =
                                    match subroutine_type_node.attr(gimli::DW_AT_name) {
                                        Ok(optional_name_attr) => match optional_name_attr {
                                            Some(name_attr) => {
                                                extract_name(self.debug_info, name_attr.value())
                                            }
                                            None => "".to_owned(),
                                        },
                                        Err(error) => {
                                            format!(
                                                "ERROR: evaluating subroutine type name: {:?} ",
                                                error
                                            )
                                        }
                                    };
                            }
                            other_attribute_value => {
                                child_variable.set_value(format!(
                                    "UNIMPLEMENTED: Attribute Value for DW_AT_type {:?}",
                                    other_attribute_value
                                ));
                            }
                        },
                        None => {
                            child_variable.set_value("<No Return Value>".to_string());
                            child_variable.type_name = "".to_string();
                        }
                    },
                    Err(error) => {
                        child_variable.set_value(format!(
                            "ERROR: Failed to decode subroutine type reference: {:?}",
                            error
                        ));
                    }
                }
            }
            gimli::DW_TAG_compile_unit => {
                // This only happens when we do a 'lazy' load of <statics>
                child_variable =
                    self.process_tree(node, child_variable, core, stack_frame_registers)?;
            }
            // Do not expand this type.
            other => {
                child_variable.type_name =
                    format!("<UNIMPLEMENTED: type : {:?}>", other.static_string());
                child_variable.set_value(child_variable.type_name.clone());
                self.debug_info
                    .variable_cache
                    .remove_cache_entry_children(child_variable.variable_key)?;
            }
        }
        self.debug_info
            .variable_cache
            .cache_variable(parent_variable.variable_key, child_variable, core)
            .map_err(|error| error.into())
    }

    /// - Consumes the `child_variable`.
    /// - Find the location using either DW_AT_location, or DW_AT_data_member_location, and store it in the Variable. A value of 0 is a valid 0 reported from dwarf.
    ///  - Returns a clone of the most up-to-date `child_variable` in the cache.
    fn extract_location(
        &self,
        node: &gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        core: &mut Core<'_>,
        stack_frame_registers: &Registers,
    ) -> Result<Variable, DebugError> {
        let mut attrs = node.entry().attrs();
        while let Some(attr) = attrs.next().unwrap() {
            match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_data_member_location
                | gimli::DW_AT_frame_base => {
                    match attr.value() {
                        gimli::AttributeValue::Exprloc(expression) => {
                            let pieces =
                                match self.expr_to_piece(core, expression, stack_frame_registers) {
                                    Ok(pieces) => pieces,
                                    Err(err) => {
                                        child_variable.set_value(format!(
                                            "ERROR: expr_to_piece() failed with: {:?}",
                                            err
                                        ));
                                        vec![]
                                    }
                                };
                            if pieces.is_empty() {
                                child_variable.memory_location = u64::MAX;
                                child_variable.set_value(format!(
                                    "ERROR: expr_to_piece() returned 0 results: {:?}",
                                    pieces
                                ));
                            } else if pieces.len() > 1 {
                                child_variable.memory_location = u64::MAX;
                                child_variable.set_value(format!("UNIMPLEMENTED: expr_to_piece() returned more than 1 result: {:?}", pieces));
                            } else {
                                match &pieces[0].location {
                                    Location::Empty => {
                                        child_variable.memory_location = 0_u64;
                                    }
                                    Location::Address { address } => {
                                        if *address == u32::MAX as u64 {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value("BUG: Cannot resolve due to rust-lang issue https://github.com/rust-lang/rust/issues/32574".to_string());
                                        } else {
                                            child_variable.memory_location = *address;
                                        }
                                    }
                                    Location::Value { value } => match value {
                                        gimli::Value::Generic(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::I8(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::U8(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::I16(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::U16(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::I32(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::U32(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::I64(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::U64(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::F32(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                        gimli::Value::F64(value) => {
                                            child_variable.memory_location = u64::MAX;
                                            child_variable.set_value(value.to_string());
                                        }
                                    },
                                    Location::Register { register } => {
                                        child_variable.memory_location = stack_frame_registers
                                            .get_value_by_dwarf_register_number(register.0 as u32)
                                            .expect("Failed to read register from `StackFrame::registers`")
                                            as u64;
                                    }
                                    l => {
                                        child_variable.memory_location = u64::MAX;
                                        child_variable.set_value(format!("UNIMPLEMENTED: extract_location() found a location type: {:?}", l));
                                    }
                                }
                            }
                        }
                        gimli::AttributeValue::Udata(offset_from_parent) => {
                            if parent_variable.memory_location != u64::MAX {
                                child_variable.memory_location =
                                    parent_variable.memory_location + offset_from_parent as u64;
                            } else {
                                child_variable.memory_location = offset_from_parent as u64;
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(format!(
                                "ERROR: extract_location() Could not extract location from: {:?}",
                                other_attribute_value
                            ));
                        }
                    }
                }
                gimli::DW_AT_address_class => {
                    match attr.value() {
                        gimli::AttributeValue::AddressClass(address_class) => {
                            // Nothing to do in this case where it is zero
                            if address_class != gimli::DwAddr(0) {
                                child_variable.set_value(format!(
                                    "UNIMPLEMENTED: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({:?}))",
                                    address_class
                                ));
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(format!(
                                "UNIMPLEMENTED: extract_location() found invalid DW_AT_address_class: {:?}",
                                other_attribute_value
                            ));
                        }
                    }
                }
                _other_attributes => {
                    // These will be handled elsewhere.
                }
            }
        }
        // If the `memory_location` is still 0 at this time, then we inherit from the parent.
        if child_variable.memory_location.is_zero()
            && !(parent_variable.memory_location.is_zero()
                || parent_variable.memory_location == u64::MAX)
        {
            child_variable.memory_location = parent_variable.memory_location;
        }
        self.debug_info
            .variable_cache
            .cache_variable(child_variable.parent_key, child_variable, core)
            .map_err(|error| error.into())
    }
}

/// If file information is available, it returns `Some(directory:PathBuf, file_name:String)`, otherwise `None`.
fn extract_file(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<GimliReader>,
    attribute_value: gimli::AttributeValue<GimliReader>,
) -> Option<(PathBuf, String)> {
    match attribute_value {
        gimli::AttributeValue::FileIndex(index) => unit.line_program.as_ref().and_then(|ilnp| {
            let header = ilnp.header();
            header.file(index).and_then(|file_entry| {
                file_entry.directory(header).map(|directory| {
                    (
                        PathBuf::from(extract_name(debug_info, directory)),
                        extract_name(debug_info, file_entry.path_name()),
                    )
                })
            })
        }),
        _ => None,
    }
}

/// If a DW_AT_byte_size attribute exists, return the u64 value, otherwise (including errors) return 0
fn extract_byte_size(
    _debug_info: &DebugInfo,
    di_entry: &DebuggingInformationEntry<GimliReader>,
) -> u64 {
    match di_entry.attr(gimli::DW_AT_byte_size) {
        Ok(optional_byte_size_attr) => match optional_byte_size_attr {
            Some(byte_size_attr) => match byte_size_attr.value() {
                gimli::AttributeValue::Udata(byte_size) => byte_size,
                other => {
                    log::warn!("UNIMPLEMENTED: DW_AT_byte_size value: {:?} ", other);
                    0
                }
            },
            None => 0,
        },
        Err(error) => {
            log::warn!(
                "Failed to extract byte_size: {:?} for debug_entry {:?}",
                error,
                di_entry.tag().static_string()
            );
            0
        }
    }
}
fn extract_line(
    _debug_info: &DebugInfo,
    attribute_value: gimli::AttributeValue<GimliReader>,
) -> Option<u64> {
    match attribute_value {
        gimli::AttributeValue::Udata(line) => Some(line),
        _ => None,
    }
}

fn extract_name(
    debug_info: &DebugInfo,
    attribute_value: gimli::AttributeValue<GimliReader>,
) -> String {
    match attribute_value {
        gimli::AttributeValue::DebugStrRef(name_ref) => {
            let name_raw = debug_info.dwarf.string(name_ref).unwrap();
            String::from_utf8_lossy(&name_raw).to_string()
        }
        gimli::AttributeValue::String(name) => String::from_utf8_lossy(&name).to_string(),
        other => format!("UNIMPLEMENTED: Evaluate name from {:?}", other),
    }
}

pub(crate) fn _print_all_attributes(
    core: &mut Core<'_>,
    stackframe_cfa: Option<u64>,
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    tag: &gimli::DebuggingInformationEntry<DwarfReader>,
    print_depth: usize,
) {
    let mut attrs = tag.attrs();

    while let Some(attr) = attrs.next().unwrap() {
        for _ in 0..(print_depth) {
            print!("\t");
        }
        print!("{}: ", attr.name());

        use gimli::AttributeValue::*;

        match attr.value() {
            Addr(a) => println!("{:#010x}", a),
            DebugStrRef(_) => {
                let val = dwarf.attr_string(unit, attr.value()).unwrap();
                println!("{}", std::str::from_utf8(&val).unwrap());
            }
            Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                loop {
                    use gimli::EvaluationResult::*;

                    result = match result {
                        Complete => break,
                        RequiresMemory { address, size, .. } => {
                            let mut buff = vec![0u8; size as usize];
                            core.read(address as u32, &mut buff)
                                .expect("Failed to read memory");
                            match size {
                                1 => evaluation
                                    .resume_with_memory(gimli::Value::U8(buff[0]))
                                    .unwrap(),
                                2 => {
                                    let val = u16::from(buff[0]) << 8 | u16::from(buff[1]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U16(val))
                                        .unwrap()
                                }
                                4 => {
                                    let val = u32::from(buff[0]) << 24
                                        | u32::from(buff[1]) << 16
                                        | u32::from(buff[2]) << 8
                                        | u32::from(buff[3]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U32(val))
                                        .unwrap()
                                }
                                x => {
                                    log::error!(
                                        "Requested memory with size {}, which is not supported yet.",
                                        x
                                    );
                                    unimplemented!();
                                }
                            }
                        }
                        RequiresFrameBase => evaluation
                            .resume_with_frame_base(stackframe_cfa.unwrap())
                            .unwrap(),
                        RequiresRegister {
                            register,
                            base_type,
                        } => {
                            let raw_value = core
                                .read_core_reg(register.0 as u16)
                                .expect("Failed to read memory");

                            if base_type != gimli::UnitOffset(0) {
                                unimplemented!(
                                    "Support for units in RequiresRegister request is not yet implemented."
                                )
                            }
                            evaluation
                                .resume_with_register(gimli::Value::Generic(raw_value as u64))
                                .unwrap()
                        }
                        RequiresRelocatedAddress(address_index) => {
                            if address_index.is_zero() {
                                // This is a rust-lang bug for statics ... https://github.com/rust-lang/rust/issues/32574;
                                evaluation.resume_with_relocated_address(u64::MAX).unwrap()
                            } else {
                                // Use the address_index as an offset from 0, so just pass it into the next step.
                                evaluation
                                    .resume_with_relocated_address(address_index)
                                    .unwrap()
                            }
                        }
                        x => {
                            println!("print_all_attributes {:?}", x);
                            // x
                            todo!()
                        }
                    }
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            }
            LocationListsRef(_) => {
                println!("LocationList");
            }
            DebugLocListsBase(_) => {
                println!(" LocationList");
            }
            DebugLocListsIndex(_) => {
                println!(" LocationList");
            }
            _ => {
                println!("print_all_attributes {:?}", attr.value());
            }
        }
    }
}
