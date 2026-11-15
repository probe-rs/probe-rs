use std::{collections::HashSet, ops::ControlFlow};

use gimli::{
    BaseAddresses, DebugFrame, UnwindContext, UnwindSection, UnwindTableRow, read::RegisterRule,
};
use probe_rs::{
    CoreRegister, Error, InstructionSet, MemoryInterface, RegisterRole, RegisterValue,
    architecture::riscv::registers::DWARF_CSR_BASE,
};

use super::{DebugInfo, DwarfReader, GimliReaderOffset, unknown_function_name};
use crate::{
    DebugError, DebugRegister, DebugRegisters, StackFrame,
    exception_handling::{ExceptionInfo, ExceptionInterface, ReturnAddressRecovery},
    get_object_reference,
};

impl DebugInfo {
    /// Performs the logical unwind of the stack and returns a `Vec<StackFrame>`
    /// - The first 'StackFrame' represents the frame at the current PC (program counter), and ...
    /// - Each subsequent `StackFrame` represents the **previous or calling** `StackFrame` in the call stack.
    /// - The majority of the work happens in `Unwinder::unwind_frame`, where each call
    ///   will create a `StackFrame` where possible, and update the unwind registers to prepare for
    ///   the next iteration.
    ///
    /// The unwind loop will continue until we meet one of the following conditions:
    /// - We can no longer unwind a valid PC value to be used for the next frame.
    /// - We encounter a LR register value of 0x0 or 0xFFFFFFFF (Arm 'Reset' value for that register).
    /// - We can not intelligently calculate a valid LR register value from the other registers,
    ///   or the `gimli::RegisterRule` result is a value of 0x0.
    ///   Note: [DWARF](https://dwarfstd.org) 6.4.4 - CIE defines the return register address
    ///   used in the `gimli::RegisterRule` tables for unwind operations.
    /// - The return address has an `Undefined` `gimli::RegisterRule`, which means the frame has no
    ///   caller. The innermost frame is exempt, because a leaf function keeps its return address
    ///   live in the register, so no rule is emitted to recover a value that was never saved.
    ///   This applies only where [`ExceptionInterface::return_address_recovery`] reports
    ///   [`ReturnAddressRecovery::UnwindInfo`].
    /// - Similarly, certain error conditions encountered in `StackFrameIterator` will also break out of the unwind loop.
    ///
    /// Note: In addition to populating the `StackFrame`s, this function will also
    /// populate the `DebugInfo::VariableCache` with `Variable`s for available Registers
    /// as well as static and function variables.
    /// TODO: Separate logic for stackframe creation and cache population
    pub fn unwind(
        &self,
        core: &mut impl MemoryInterface,
        initial_registers: DebugRegisters,
        exception_handler: &dyn ExceptionInterface,
        instruction_set: Option<InstructionSet>,
        max_stack_frame_count: usize,
    ) -> Result<Vec<StackFrame>, Error> {
        let mut unwinder = Unwinder {
            debug_info: self,
            memory: core,
            exception_handler,
            instruction_set,
            registers: initial_registers,
            stack_frames: Vec::new(),
            unwound_frames: 0,
        };

        let mut unwind_context = Box::new(UnwindContext::new());
        let mut visited_frames = HashSet::new();

        // Unwind [StackFrame]'s for as long as we can unwind a valid PC value.
        while let Some(frame_pc_register_value) = unwinder.program_counter() {
            if unwinder.stack_frames.len() >= max_stack_frame_count {
                tracing::warn!("Stopped unwinding the stack after {max_stack_frame_count} frames");
                break;
            }
            let frame_pc: u64 = frame_pc_register_value.try_into().map_err(|error| {
                let message = format!("Cannot convert register value for program counter to a 64-bit integer value: {error:?}");
                Error::Register(message)
            })?;

            // A valid unwind never returns to a frame it has already walked. A repeat means the
            // unwind information, or a heuristic that stood in for it, produced a return address
            // that leads back into the stack, and following it would not terminate. The stack
            // pointer is part of the identity because a recursive call legitimately revisits a PC.
            if !visited_frames.insert((frame_pc, unwinder.stack_pointer())) {
                tracing::warn!(
                    "UNWIND: Stopped unwinding at {frame_pc:#010x}, which was already unwound. \
                     The unwind information of this frame is inconsistent."
                );
                break;
            }

            let flow =
                unwinder.unwind_frame(&mut unwind_context, frame_pc_register_value, frame_pc);
            if flow.is_break() {
                break;
            }
        }

        Ok(unwinder.stack_frames)
    }
}

/// The state of a stack unwind, between two frames.
struct Unwinder<'a, M: MemoryInterface> {
    debug_info: &'a DebugInfo,
    memory: &'a mut M,
    exception_handler: &'a dyn ExceptionInterface,
    instruction_set: Option<InstructionSet>,
    /// The registers of the frame to unwind next.
    registers: DebugRegisters,
    stack_frames: Vec<StackFrame>,
    /// The number of frames that reached the return address step of the DWARF unwind.
    unwound_frames: usize,
}

/// The frame that is being unwound to its caller.
struct CalleeFrame {
    /// The program counter, as the registers hold it.
    pc_value: RegisterValue,
    pc: u64,
    /// The CFA (Canonical Frame Address), if the unwind information yields one.
    cfa: Option<u64>,
    /// The registers of this frame. They resolve [DWARF](https://dwarfstd.org) expressions while
    /// [`Unwinder::registers`] turns into the registers of the calling frame.
    registers: DebugRegisters,
}

impl<M: MemoryInterface> Unwinder<'_, M> {
    /// The program counter of the frame to unwind next, unless it is unknown or a reset value.
    fn program_counter(&self) -> Option<RegisterValue> {
        let pc = self.registers.get_program_counter()?;
        if pc.is_zero() || pc.is_max_value() {
            None
        } else {
            pc.value
        }
    }

    fn stack_pointer(&self) -> Option<u64> {
        self.registers
            .get_stack_pointer()
            .and_then(|sp| sp.value)
            .and_then(|sp| sp.try_into().ok())
    }

    /// Adds the stack frames at `frame_pc`, then unwinds the registers to the calling frame.
    ///
    /// Breaks when the unwind cannot continue past this frame.
    fn unwind_frame(
        &mut self,
        unwind_context: &mut UnwindContext<GimliReaderOffset>,
        frame_pc_register_value: RegisterValue,
        frame_pc: u64,
    ) -> ControlFlow<()> {
        // PART 1: Construct the `StackFrame`s for the current program counter.
        //
        //         Multiple stack frames can be constructed if we are inside inlined functions.
        tracing::trace!(
            "UNWIND: Will generate `StackFrame` for function at address (PC) {frame_pc_register_value:#}"
        );
        let unwind_info = get_unwind_info(unwind_context, &self.debug_info.frame_section, frame_pc);

        // Determining the frame base may need the CFA (Canonical Frame Address) to be calculated first.
        let cfa = unwind_info
            .as_ref()
            .ok()
            .map(|(unwind_info, _)| determine_cfa(&self.registers, unwind_info));
        let (cfa, cfa_unsupported) = match cfa {
            Some(Ok(DwarfUnwind::Value(cfa))) => (cfa, false),
            Some(Ok(DwarfUnwind::UnsupportedCfi)) => (None, true),
            Some(Err(_)) | None => (None, false),
        };

        // Because we will be updating the `registers` with previous frame unwind info,
        // we need to keep a copy of the current frame's registers that can be used to resolve [DWARF](https://dwarfstd.org) expressions.
        let frame = CalleeFrame {
            pc_value: frame_pc_register_value,
            pc: frame_pc,
            cfa,
            registers: self.registers.clone(),
        };

        // PART 1-a: Prepare the `StackFrame`s that holds the current frame information.
        self.push_stack_frames(&frame)?;

        // PART 2: Setup the registers for the next iteration (a.k.a. unwind previous frame, a.k.a. "callee", in the call stack).
        tracing::trace!("UNWIND - Preparing to unwind the registers for the previous frame.");

        // PART 2-a: get the `gimli::FrameDescriptorEntry` for the program counter
        // and then the unwind info associated with this row.
        let unwind_info = match unwind_info {
            Ok((unwind_info, cie_return_address)) if !cfa_unsupported => {
                tracing::trace!("UNWIND: Found unwind info for address {frame_pc:#010x}");
                Some((unwind_info, cie_return_address))
            }
            Ok(_) => None,
            Err(err) => {
                tracing::trace!(
                    "UNWIND: Unable to find unwind info for address {frame_pc:#010x}: {err}"
                );
                None
            }
        };

        let Some((unwind_info, cie_return_address)) = unwind_info else {
            return self.fall_back_to_heuristic_unwind(&frame);
        };

        // PART 2-b: Unwind registers for the "previous/calling" frame.
        if self.unwind_registers(&frame, unwind_info)? == DwarfUnwind::UnsupportedCfi {
            tracing::warn!(
                "UNWIND: Falling back to unwinding without debug info at {frame_pc:#010x}"
            );
            self.registers.clone_from(&frame.registers);
            return self.fall_back_to_heuristic_unwind(&frame);
        }

        // PART 3: Check if we entered the current frame from an exception handler.
        // - If we are at an exception handler frame:
        //   - Create a "handler" stackframe that can be inserted into the stack_frames list,
        //     instead of "unknown function @ address";
        //   - Overwrite the unwind registers with the exception context.
        // - If for some reason we cannot determine the exception context, we silently continue with the rest of the unwind.
        // At worst, the unwind will be able to unwind the stack to the frame of the most recent exception handler.
        if let Some(flow) = self.unwind_into_exception_handler(&frame) {
            return flow;
        }

        self.unwind_program_counter(&frame, unwind_info, cie_return_address)
    }

    /// Adds the stack frames of `frame`, the innermost inlined function first.
    fn push_stack_frames(&mut self, frame: &CalleeFrame) -> ControlFlow<()> {
        let cached_stack_frames = match self.debug_info.get_stackframe_info(
            self.memory,
            frame.pc,
            frame.cfa,
            &self.registers,
        ) {
            Ok(cached_stack_frames) => cached_stack_frames,
            Err(e) => {
                tracing::error!("UNWIND: Unable to complete `StackFrame` information: {e}");
                // There is no point in continuing with the unwind, so let's get out of here.
                return ControlFlow::Break(());
            }
        };

        if cached_stack_frames.is_empty() {
            // We have no valid code for the current frame, so we
            // construct a frame, using what information we have.
            self.stack_frames.push(StackFrame {
                id: get_object_reference(),
                function_name: unknown_function_name(frame.pc, &self.registers),
                source_location: self.debug_info.get_source_location(frame.pc),
                registers: self.registers.clone(),
                pc: frame.pc_value,
                frame_base: None,
                is_inlined: false,
                local_variables: None,
                canonical_frame_address: None,
            });
            return ControlFlow::Continue(());
        }

        // Add the found stackframes to the list, in reverse order. `get_stackframe_info` returns the frames in
        // the order of the most recently called function last, but the stack frames should be
        // in the order of the most recently called function first.
        for stack_frame in cached_stack_frames.into_iter().rev() {
            if stack_frame.is_inlined {
                tracing::trace!(
                    "UNWIND: Found inlined function - name={}, pc={}",
                    stack_frame.function_name,
                    stack_frame.pc
                );
            }
            self.stack_frames.push(stack_frame);
        }
        ControlFlow::Continue(())
    }

    /// Unwinds every register but the program counter with the DWARF rules in `unwind_info`.
    ///
    /// Returns [`DwarfUnwind::UnsupportedCfi`] when a rule needs the unwind without debug info.
    fn unwind_registers(
        &mut self,
        frame: &CalleeFrame,
        unwind_info: &UnwindTableRow<GimliReaderOffset>,
    ) -> ControlFlow<(), DwarfUnwind<()>> {
        for debug_register in self.registers.0.iter_mut() {
            let register_rule = dwarf_register_rule(debug_register, unwind_info);

            // The program counter is handled later.
            let is_program_counter = debug_register
                .core_register
                .register_has_role(RegisterRole::ProgramCounter);
            if is_program_counter && cfi_register_rule_unsupported(&register_rule) {
                tracing::warn!(
                    "UNWIND: Unsupported CFI register rule {register_rule:?} for register {}",
                    debug_register.core_register
                );
                return ControlFlow::Continue(DwarfUnwind::UnsupportedCfi);
            }
            if is_program_counter {
                continue;
            }

            match unwind_register_using_rule(
                debug_register.core_register,
                &frame.registers,
                frame.cfa,
                self.memory,
                register_rule,
                self.exception_handler,
            ) {
                Err(error) => {
                    tracing::warn!(
                        "Failed to unwind register {} for frame at {:#010x}: {error}",
                        debug_register.core_register,
                        frame.pc
                    );
                    append_to_function_name(
                        self.stack_frames.last_mut(),
                        " (unwinding stopped due to an error)",
                    );
                    return ControlFlow::Break(());
                }
                Ok(DwarfUnwind::Value(val)) => debug_register.value = val,
                Ok(DwarfUnwind::UnsupportedCfi) => {
                    return ControlFlow::Continue(DwarfUnwind::UnsupportedCfi);
                }
            };
        }

        ControlFlow::Continue(DwarfUnwind::Value(()))
    }

    /// Unwinds `frame` with the heuristics of the architecture, for a frame that has no usable
    /// unwind information.
    fn fall_back_to_heuristic_unwind(&mut self, frame: &CalleeFrame) -> ControlFlow<()> {
        self.unwind_frame_without_debuginfo(frame.pc)?;

        if frame.registers == self.registers {
            tracing::debug!("No change, preventing infinite loop");
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }

    fn unwind_frame_without_debuginfo(&mut self, frame_pc: u64) -> ControlFlow<()> {
        // Check for an exception before the heuristic unwind, which would otherwise overwrite the
        // registers that identify the exception with a guess.
        match self.exception_details() {
            Ok(Some(exception_info)) => {
                tracing::trace!(
                    "UNWIND: Stack unwind reached an exception handler {} (no debug info path)",
                    exception_info.description
                );
                self.enter_exception_handler(exception_info);
                return ControlFlow::Continue(());
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "UNWIND: Error checking exception context (no debug info path): {e:?}"
                );
            }
        }

        let ControlFlow::Break(error) = self.exception_handler.unwind_without_debuginfo(
            &mut self.registers,
            frame_pc,
            &self.stack_frames,
            self.instruction_set,
            self.memory,
        ) else {
            return ControlFlow::Continue(());
        };

        if let Some(error) = error {
            // This is not fatal, but we cannot continue unwinding beyond the current frame.
            tracing::error!("{:?}", &error);
            append_to_function_name(
                self.stack_frames.first_mut(),
                &format!(" : ERROR : {error}"),
            );
        }
        ControlFlow::Break(())
    }

    /// Continues the unwind in the exception handler that `frame` was entered from.
    ///
    /// Returns `None` when `frame` was not entered from an exception handler.
    fn unwind_into_exception_handler(&mut self, frame: &CalleeFrame) -> Option<ControlFlow<()>> {
        if !self.has_return_address() {
            return None;
        }

        match self.exception_details() {
            Ok(Some(exception_info)) => {
                tracing::trace!(
                    "UNWIND: Stack unwind reached an exception handler {}",
                    exception_info.description
                );
                self.enter_exception_handler(exception_info);
                // We have everything we need to unwind the next frame in the stack.
                Some(ControlFlow::Continue(()))
            }
            Ok(None) => {
                tracing::trace!("UNWIND: No exception context found. Stack unwind will continue.");
                None
            }
            Err(e) => {
                // TODO: Nicely print error with sources
                let message = format!(
                    "UNWIND: Error while checking for exception context. The stack trace will not include the calling frames.\n{e:?}"
                );
                tracing::warn!("{message}");
                self.stack_frames.push(StackFrame {
                    id: get_object_reference(),
                    function_name: message,
                    source_location: None,
                    registers: self.registers.clone(),
                    pc: frame.pc_value,
                    frame_base: None,
                    is_inlined: false,
                    local_variables: None,
                    canonical_frame_address: None,
                });
                Some(ControlFlow::Break(()))
            }
        }
    }

    /// The exception that the frame in `registers` was entered from, if its return address is known.
    fn exception_details(&mut self) -> Result<Option<ExceptionInfo>, DebugError> {
        if !self.has_return_address() {
            return Ok(None);
        }

        self.exception_handler
            .exception_details(self.memory, &self.registers, self.debug_info)
    }

    fn has_return_address(&self) -> bool {
        self.registers
            .get_return_address()
            .is_some_and(|ra| ra.value.is_some())
    }

    fn enter_exception_handler(&mut self, exception_info: ExceptionInfo) {
        self.registers = exception_info.handler_frame.registers.clone();
        self.stack_frames.push(exception_info.handler_frame);
    }

    /// Sets the program counter of the calling frame from the return address of `frame`.
    ///
    /// Breaks when `frame` has no caller.
    fn unwind_program_counter(
        &mut self,
        frame: &CalleeFrame,
        unwind_info: &UnwindTableRow<GimliReaderOffset>,
        cie_return_address: gimli::Register,
    ) -> ControlFlow<()> {
        let (unwound_return_address, return_address_source) = caller_return_address(
            &frame.registers,
            &self.registers,
            unwind_info,
            cie_return_address,
            self.exception_handler.return_address_recovery(),
        );

        // DWARF 6.4.4: no rule for the return address means the frame has no caller. The
        // innermost frame is exempt, because a leaf function keeps its return address live in
        // the register and compilers emit no rule to recover what was never saved.
        if return_address_source == ReturnAddressSource::Undefined && self.unwound_frames > 0 {
            tracing::debug!(
                "UNWIND: Stopped unwinding at {:#010x}, which has no rule for its return address.",
                frame.pc
            );
            return ControlFlow::Break(());
        }
        self.unwound_frames += 1;

        let program_counter = self.registers.get_program_counter_mut().unwrap();

        let Ok(current_pc) = frame
            .registers
            .get_register_value_by_role(&RegisterRole::ProgramCounter)
        else {
            let error = "UNWIND: Tried to unwind return address value where current program counter is unknown.";
            tracing::error!("{error}");
            append_to_function_name(self.stack_frames.last_mut(), &format!(" : ERROR: {error}"));
            return ControlFlow::Break(());
        };
        // NOTE: PC = Value of the unwound LR, i.e. the first instruction after the one that called this function.
        // If both the LR and PC registers have undefined rules, this will prevent the unwind from continuing.
        program_counter.value = unwound_return_address.and_then(|return_address| {
            unwind_program_counter_register(
                return_address,
                current_pc,
                self.instruction_set,
                return_address_source == ReturnAddressSource::Csr,
            )
        });

        ControlFlow::Continue(())
    }
}

/// Appends `suffix` to the function name of `stack_frame`, to show why the unwind stopped there.
fn append_to_function_name(stack_frame: Option<&mut StackFrame>, suffix: &str) {
    if let Some(stack_frame) = stack_frame {
        stack_frame.function_name.push_str(suffix);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DwarfUnwind<T> {
    Value(T),
    UnsupportedCfi,
}

fn is_frame_walk_register(register: &CoreRegister) -> bool {
    register.register_has_role(RegisterRole::ProgramCounter)
        || register.register_has_role(RegisterRole::StackPointer)
        || register.register_has_role(RegisterRole::ReturnAddress)
}

/// Must list the same rules that [`unwind_register_using_rule`] implements.
fn cfi_register_rule_unsupported<T: gimli::ReaderOffset>(rule: &RegisterRule<T>) -> bool {
    !matches!(
        rule,
        RegisterRule::Undefined
            | RegisterRule::SameValue
            | RegisterRule::Offset(_)
            | RegisterRule::ValOffset(_)
            | RegisterRule::Register(_)
            | RegisterRule::Constant(_)
    )
}

fn dwarf_register_rule(
    debug_register: &DebugRegister,
    unwind_info: &UnwindTableRow<GimliReaderOffset>,
) -> RegisterRule<GimliReaderOffset> {
    debug_register
        .dwarf_id
        .and_then(|register_position| unwind_info.register(gimli::Register(register_position)))
        .unwrap_or(RegisterRule::Undefined)
}

/// Get a handle to the [`gimli::UnwindTableRow`] for this call frame, so that we can reference it to unwind register values.
pub fn get_unwind_info<'a>(
    unwind_context: &'a mut UnwindContext<GimliReaderOffset>,
    frame_section: &DebugFrame<DwarfReader>,
    frame_program_counter: u64,
) -> Result<(&'a UnwindTableRow<GimliReaderOffset>, gimli::Register), DebugError> {
    let transform_error = |error| {
        DebugError::Other(format!(
            "UNWIND: Error reading FrameDescriptorEntry at PC={frame_program_counter:x} : {error}"
        ))
    };

    let unwind_bases = BaseAddresses::default();

    let frame_descriptor_entry = frame_section
        .fde_for_address(
            &unwind_bases,
            frame_program_counter,
            DebugFrame::cie_from_offset,
        )
        .map_err(transform_error)?;

    let return_address_register = frame_descriptor_entry.cie().return_address_register();

    let unwind_info = frame_descriptor_entry
        .unwind_info_for_address(
            frame_section,
            &unwind_bases,
            unwind_context,
            frame_program_counter,
        )
        .map_err(transform_error)?;

    Ok((unwind_info, return_address_register))
}

/// Determines the CFA (canonical frame address) for the current [`gimli::UnwindTableRow`], using the current register values.
fn determine_cfa<R: gimli::ReaderOffset>(
    unwind_registers: &DebugRegisters,
    unwind_info: &UnwindTableRow<R>,
) -> Result<DwarfUnwind<Option<u64>>, Error> {
    let gimli::CfaRule::RegisterAndOffset { register, offset } = unwind_info.cfa() else {
        tracing::warn!("UNWIND: Unsupported CFI CFA rule {:?}", unwind_info.cfa());
        return Ok(DwarfUnwind::UnsupportedCfi);
    };

    let reg_val = unwind_registers
        .get_register_by_dwarf_id(register.0)
        .and_then(|register| register.value);

    let cfa = match reg_val {
        None => {
            tracing::error!(
                "UNWIND: `StackFrameIterator` unable to determine the unwind CFA: Missing value of register {}",
                register.0
            );
            None
        }

        Some(reg_val) if reg_val.is_zero() => {
            // If we encounter this rule for CFA, it implies the scenario depends on a FP/frame pointer to continue successfully.
            // Therefore, if reg_val is zero (i.e. FP is zero), then we do not have enough information to determine the CFA by rule.
            tracing::trace!(
                "UNWIND: Stack unwind complete - The FP register value unwound to a value of zero."
            );
            None
        }

        Some(reg_val) => {
            let unwind_cfa = add_to_address(
                reg_val.try_into()?,
                *offset,
                unwind_registers.get_address_size_bytes(),
            );
            tracing::trace!(
                "UNWIND - CFA : {:#010x}\tRule: {:?}",
                unwind_cfa,
                unwind_info.cfa()
            );
            Some(unwind_cfa)
        }
    };

    Ok(DwarfUnwind::Value(cfa))
}

/// Unwind the program counter for the caller frame, using the LR value from the callee frame.
pub fn unwind_pc_without_debuginfo(
    unwind_registers: &mut DebugRegisters,
    _frame_pc: u64,
    instruction_set: Option<InstructionSet>,
) -> ControlFlow<Option<DebugError>> {
    // For non exception frames, we cannot do stack unwinding if we do not have debug info.
    // However, there is one case where we can continue. When the frame registers have a valid
    // return address/LR value, we can use the LR value to calculate the PC for the calling frame.
    // The current logic will then use that PC to get the next frame's unwind info, and if that exists,
    // we will be able to continue unwinding.
    // If the calling frame has no debug info, then the unwinding will end with that frame.
    let callee_frame_registers = unwind_registers.clone();
    let unwound_return_address: Option<RegisterValue> = unwind_registers
        .get_return_address()
        .and_then(|lr| lr.value);

    // This will update the program counter in the `unwind_registers` with the PC value calculated from the LR value.
    if let Some(calling_pc) = unwind_registers.get_program_counter_mut() {
        let Ok(current_pc) =
            callee_frame_registers.get_register_value_by_role(&RegisterRole::ProgramCounter)
        else {
            return ControlFlow::Break(
                Some(Error::Other(
                    "UNWIND: Tried to unwind return address value where current program counter is unknown.".to_string()
                ).into())
            );
        };
        // NOTE: PC = Value of the unwound LR, i.e. the first instruction after the one that called this function.
        // If both the LR and PC registers have undefined rules, this will prevent the unwind from continuing.
        calling_pc.value = unwound_return_address.and_then(|return_address| {
            unwind_program_counter_register(return_address, current_pc, instruction_set, false)
        })
    }

    ControlFlow::Continue(())
}

/// A per_register unwind, applying `register_rule` to compute the value of `debug_register` in the calling frame.
fn unwind_register_using_rule(
    debug_register: &CoreRegister,
    // The callee_frame_registers are used to lookup values and never updated.
    callee_frame_registers: &DebugRegisters,
    unwind_cfa: Option<u64>,
    memory: &mut dyn MemoryInterface,
    register_rule: RegisterRule<usize>,
    exception_handler: &dyn ExceptionInterface,
) -> Result<DwarfUnwind<Option<RegisterValue>>, Error> {
    let mut register_rule_string = format!("{register_rule:?}");
    let address_size = callee_frame_registers.get_address_size_bytes();

    let new_value = match register_rule {
        RegisterRule::Undefined => exception_handler
            .unwind_undefined_register(
                debug_register,
                callee_frame_registers,
                unwind_cfa,
                memory,
                &mut register_rule_string,
            )
            .map_err(|e| match e {
                DebugError::Probe(err) => err,
                other => Error::Other(other.to_string()),
            })?,

        RegisterRule::SameValue => callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value),

        RegisterRule::Offset(address_offset) => {
            // "The previous value of this register is saved at the address CFA+N where CFA is the current CFA value and N is a signed offset"
            let unwind_cfa = required_cfa(unwind_cfa)?;
            let previous_frame_register_address =
                add_to_address(unwind_cfa, address_offset, address_size);

            register_rule_string = format!("CFA {register_rule:?}");

            let value = read_saved_register(
                memory,
                debug_register,
                previous_frame_register_address,
                address_size,
            )
            .inspect_err(|_| {
                tracing::error!(
                    "UNWIND: Rule: Offset {} from address {:#010x}",
                    address_offset,
                    unwind_cfa
                );
            })?;
            Some(value)
        }

        RegisterRule::Register(source) => callee_frame_registers
            .get_register_by_dwarf_id(source.0)
            .and_then(|reg| reg.value),

        RegisterRule::ValOffset(address_offset) => {
            let unwind_cfa = required_cfa(unwind_cfa)?;
            let value = add_to_address(unwind_cfa, address_offset, address_size);

            register_rule_string = format!("CFA {register_rule:?}");

            Some(address_sized_value(value, address_size)?)
        }

        RegisterRule::Constant(value) => Some(address_sized_value(value, address_size)?),

        other => {
            tracing::warn!(
                "UNWIND: Unsupported CFI register rule {other:?} for register {debug_register}"
            );
            if is_frame_walk_register(debug_register) {
                return Ok(DwarfUnwind::UnsupportedCfi);
            }
            None
        }
    };

    tracing::trace!(
        "UNWIND - {:>10}: Caller: {}\tCallee: {}\tRule: {}",
        debug_register,
        new_value.unwrap_or_default(),
        callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value)
            .unwrap_or_default(),
        register_rule_string,
    );
    Ok(DwarfUnwind::Value(new_value))
}

fn required_cfa(unwind_cfa: Option<u64>) -> Result<u64, Error> {
    unwind_cfa.ok_or_else(|| {
        Error::Other("UNWIND: Tried to unwind `RegisterRule` at CFA = None.".to_string())
    })
}

/// Reads the value of `debug_register` that the callee frame saved at `address`.
fn read_saved_register(
    memory: &mut dyn MemoryInterface,
    debug_register: &CoreRegister,
    address: u64,
    address_size: usize,
) -> Result<RegisterValue, Error> {
    // TODO: This should be the size of the register, not the address size.
    let result = match address_size {
        4 => {
            let mut buff = [0u8; 4];
            memory
                .read(address, &mut buff)
                .map(|_| RegisterValue::U32(u32::from_le_bytes(buff)))
        }
        8 => {
            let mut buff = [0u8; 8];
            memory
                .read(address, &mut buff)
                .map(|_| RegisterValue::U64(u64::from_le_bytes(buff)))
        }
        _ => return Err(unsupported_address_size(address_size)),
    };

    result.map_err(|error| {
        Error::Other(format!(
            "UNWIND: Failed to read value for register {} from address {} ({} bytes): {}",
            debug_register,
            RegisterValue::from(address),
            4,
            error
        ))
    })
}

/// Truncates `value` to a [`RegisterValue`] of `address_size` bytes.
fn address_sized_value(value: u64, address_size: usize) -> Result<RegisterValue, Error> {
    match address_size {
        4 => Ok(RegisterValue::U32(value as u32)),
        8 => Ok(RegisterValue::U64(value)),
        _ => Err(unsupported_address_size(address_size)),
    }
}

fn unsupported_address_size(address_size: usize) -> Error {
    Error::Other(format!(
        "UNWIND: Address size {address_size} not supported."
    ))
}

fn is_csr_dwarf_column(dwarf_id: u16) -> bool {
    dwarf_id >= DWARF_CSR_BASE
}

/// Where the return address of the calling frame came from.
#[derive(Debug, PartialEq, Eq)]
enum ReturnAddressSource {
    /// A CSR, either named by the CIE's return column or by a `Register` rule on the return
    /// address. Such a value already points at the interrupted instruction.
    Csr,
    /// The return address register, with a DWARF rule to recover it.
    Register,
    /// DWARF has no rule for the return address.
    Undefined,
}

fn caller_return_address(
    callee_frame_registers: &DebugRegisters,
    unwind_registers: &DebugRegisters,
    unwind_info: &gimli::UnwindTableRow<GimliReaderOffset>,
    cie_return_address: gimli::Register,
    recovery: ReturnAddressRecovery,
) -> (Option<RegisterValue>, ReturnAddressSource) {
    if recovery == ReturnAddressRecovery::CalledFrameRegister {
        // `unwind_registers` already holds the return address of the calling frame, which belongs
        // to the frame above the one whose program counter is wanted here.
        let ra = callee_frame_registers
            .get_return_address()
            .and_then(|ra| ra.value);
        return (ra, ReturnAddressSource::Register);
    }

    let ra = unwind_registers.get_return_address();

    // Xtensa CIEs name DWARF column 1 as the return address, but that column is SP in
    // probe-rs. Honour a non-RA CIE column only when it is a CSR (`.cfi_return_column 0x1341`).
    if is_csr_dwarf_column(cie_return_address.0)
        && let Some(reg) = unwind_registers.get_register_by_dwarf_id(cie_return_address.0)
    {
        return (reg.value, ReturnAddressSource::Csr);
    }

    let source = match ra.map(|ra| dwarf_register_rule(ra, unwind_info)) {
        Some(RegisterRule::Register(source)) if is_csr_dwarf_column(source.0) => {
            ReturnAddressSource::Csr
        }
        Some(RegisterRule::Undefined) | None => ReturnAddressSource::Undefined,
        Some(_) => ReturnAddressSource::Register,
    };
    (ra.and_then(|reg| reg.value), source)
}

/// Helper function to determine the program counter value for the previous frame.
pub fn unwind_program_counter_register(
    return_address: RegisterValue,
    current_pc: u64,
    instruction_set: Option<InstructionSet>,
    from_csr: bool,
) -> Option<RegisterValue> {
    if return_address.is_max_value() || return_address.is_zero() {
        tracing::debug!(
            "No reliable return address is available, so we cannot determine the program counter to unwind the previous frame."
        );
        return None;
    }

    const DEFAULT_REGISTER_RULE_STR: &str = "PC=(unwound LR) (dwarf Undefined)";

    // A CSR-sourced return address (for example `mepc`) already points at the interrupted instruction.
    if from_csr {
        tracing::trace!(
            "UNWIND - PC: Caller: {return_address}\tCallee: {current_pc:#010x}\tRule: PC=(unwound CSR)"
        );
        return Some(return_address);
    }

    let (caller_pc, rule_str) = match return_address {
        RegisterValue::U32(return_address) => {
            match instruction_set {
                Some(InstructionSet::Thumb2) => {
                    // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section A5.1.2:
                    //
                    // We have to clear the last bit to ensure the PC is half-word aligned. (on ARM architecture,
                    // when in Thumb state for certain instruction types will set the LSB to 1)
                    (
                        Some(RegisterValue::U32((return_address - 2) & !0b1)),
                        "PC=(unwound (LR - 2) & !0b1) (dwarf Undefined)",
                    )
                }
                Some(InstructionSet::Riscv { c: true, .. }) => (
                    Some(RegisterValue::U32(return_address - 2)),
                    "PC=(unwound x1 - 2) (dwarf Undefined)",
                ),
                Some(InstructionSet::Riscv { c: false, .. }) => (
                    Some(RegisterValue::U32(return_address - 4)),
                    "PC=(unwound x1 - 4) (dwarf Undefined)",
                ),
                Some(InstructionSet::Xtensa) => {
                    let upper_bits = (current_pc as u32) & 0xC000_0000;
                    (
                        Some(RegisterValue::U32(
                            (return_address & 0x3FFF_FFFF | upper_bits) - 3,
                        )),
                        "PC=(unwound x0 - 3) (dwarf Undefined)",
                    )
                }
                _ => (
                    Some(RegisterValue::U32(return_address)),
                    DEFAULT_REGISTER_RULE_STR,
                ),
            }
        }
        RegisterValue::U64(return_address) => (
            Some(RegisterValue::U64(return_address)),
            DEFAULT_REGISTER_RULE_STR,
        ),
        RegisterValue::U128(_) => {
            tracing::warn!("128 bit address space not supported");
            (None, "PC=(undefined) (dwarf Undefined)")
        }
    };

    tracing::trace!(
        "UNWIND - PC: Caller: {}\tCallee: {:#010x}\tRule: {}",
        caller_pc.unwrap_or_default(),
        current_pc,
        rule_str,
    );

    caller_pc
}

/// Helper function to handle adding a signed offset to a [`RegisterValue`] address.
/// The numerical overflow is handled based on the byte size (`address_size_in_bytes` parameter  )
/// of the [`RegisterValue`], as opposed to just the datatype of the `address` parameter.
/// In the case of unwinding stack frame register values, it makes no sense to wrap,
/// because it will result in invalid register address reads.
/// Instead, when we detect over/underflow, we return an address value of 0x0,
/// which will trigger a graceful (and logged) end of a stack unwind.
fn add_to_address(address: u64, offset: i64, address_size_in_bytes: usize) -> u64 {
    match address_size_in_bytes {
        4 => {
            if offset >= 0 {
                (address as u32)
                    .checked_add(offset as u32)
                    .map(u64::from)
                    .unwrap_or(0x0)
            } else {
                (address as u32).saturating_sub(offset.unsigned_abs() as u32) as u64
            }
        }
        8 => {
            if offset >= 0 {
                address.checked_add(offset as u64).unwrap_or(0x0)
            } else {
                address.saturating_sub(offset.unsigned_abs())
            }
        }
        _ => {
            panic!(
                "UNWIND: Address size {address_size_in_bytes} not supported.  Please report this as a bug."
            );
        }
    }
}

#[cfg(test)]
mod test {
    use gimli::RegisterRule;
    use probe_rs::{
        InstructionSet, RegisterRole, RegisterValue, Xlen,
        architecture::{
            arm::core::registers::cortex_m::{self, CORTEX_M_CORE_REGISTERS},
            riscv::registers::RISCV_CORE_REGISTERS,
        },
        test::MockMemory,
    };

    use super::{DwarfUnwind, unwind_program_counter_register, unwind_register_using_rule};
    use crate::{
        DebugRegister, DebugRegisters,
        exception_handling::{armv7m::ArmV7MExceptionHandler, riscv::RiscvExceptionHandler},
    };

    #[test]
    fn unwind_same_value() {
        let rule = gimli::RegisterRule::SameValue;

        let mut callee_frame_registers = DebugRegisters::default();
        let debug_register = CORTEX_M_CORE_REGISTERS.core_registers().next().unwrap();

        let expected_value = Some(RegisterValue::U32(0x1234));

        callee_frame_registers.0.push(DebugRegister {
            core_register: debug_register,
            dwarf_id: Some(0),
            value: expected_value,
        });

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            debug_register,
            &callee_frame_registers,
            None,
            &mut memory,
            rule,
            &ArmV7MExceptionHandler,
        )
        .unwrap();

        assert_eq!(value, DwarfUnwind::Value(expected_value));
    }

    #[test]
    fn unwind_offset() {
        let cfa = 0x1000;
        let offset = 4;
        let rule = gimli::RegisterRule::Offset(offset as i64);
        let expected_value = 0xcafe;

        let expected_register_value = Some(RegisterValue::U32(expected_value));

        let mut memory = MockMemory::new();
        memory.add_word_range(cfa + offset, &[expected_value]);

        let mut callee_frame_registers = DebugRegisters::default();
        let debug_register = CORTEX_M_CORE_REGISTERS.core_registers().next().unwrap();

        callee_frame_registers.0.push(DebugRegister {
            core_register: debug_register,
            dwarf_id: Some(0),
            value: None,
        });

        // This is necessary for the unwind code to determine the address size of the system
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::PC,
            dwarf_id: Some(15),
            value: Some(RegisterValue::U32(0x0)),
        });

        let value = unwind_register_using_rule(
            debug_register,
            &callee_frame_registers,
            Some(cfa),
            &mut memory,
            rule,
            &ArmV7MExceptionHandler,
        )
        .unwrap();

        assert_eq!(value, DwarfUnwind::Value(expected_register_value));
    }

    #[test]
    fn unwind_undefined_for_frame_pointer() {
        let mut callee_frame_registers = DebugRegisters::default();
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::FP,
            dwarf_id: Some(7),
            value: Some(RegisterValue::U32(0x100)),
        });

        // This is necessary for the unwind code to determine the address size of the system
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::PC,
            dwarf_id: Some(15),
            value: Some(RegisterValue::U32(0x0)),
        });

        let cfa = 0x200;

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            &cortex_m::FP,
            &callee_frame_registers,
            Some(cfa),
            &mut memory,
            RegisterRule::Undefined,
            &ArmV7MExceptionHandler,
        )
        .unwrap();

        // R7/FP is callee-saved and has `UnwindRule::Preserve`, so when DWARF has
        // no rule for it the caller's value is the callee value (0x100) carried
        // forward, NOT the canonical frame address. Overwriting it with the CFA
        // would corrupt the frame pointer when unwinding handlers that don't save R7.
        assert_eq!(value, DwarfUnwind::Value(Some(RegisterValue::U32(0x100))));
    }

    #[test]
    fn unwind_register_from_mepc() {
        let mepc = RISCV_CORE_REGISTERS
            .core_registers()
            .find(|register| register.dwarf_id() == Some(4929))
            .unwrap();
        let ra = RISCV_CORE_REGISTERS
            .core_registers()
            .find(|register| register.register_has_role(RegisterRole::ReturnAddress))
            .unwrap();
        let pc = RISCV_CORE_REGISTERS
            .core_registers()
            .find(|register| register.register_has_role(RegisterRole::ProgramCounter))
            .unwrap();

        let expected_value = Some(RegisterValue::U32(0x8000_1234));

        let mut callee_frame_registers = DebugRegisters::default();
        callee_frame_registers.0.push(DebugRegister {
            core_register: mepc,
            dwarf_id: mepc.dwarf_id(),
            value: expected_value,
        });
        callee_frame_registers.0.push(DebugRegister {
            core_register: ra,
            dwarf_id: ra.dwarf_id(),
            value: Some(RegisterValue::U32(0x0)),
        });
        callee_frame_registers.0.push(DebugRegister {
            core_register: pc,
            dwarf_id: pc.dwarf_id(),
            value: Some(RegisterValue::U32(0x0)),
        });

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            ra,
            &callee_frame_registers,
            None,
            &mut memory,
            RegisterRule::Register(gimli::Register(4929)),
            &RiscvExceptionHandler,
        )
        .unwrap();

        assert_eq!(value, DwarfUnwind::Value(expected_value));
    }

    #[test]
    fn unwind_pc_from_csr_skips_call_adjustment() {
        let mepc = RegisterValue::U32(0x8000_1234);
        let rv32 = Some(InstructionSet::Riscv {
            xlen: Xlen::X32,
            c: false,
            f: false,
            e: false,
        });

        assert_eq!(
            unwind_program_counter_register(mepc, 0x8000_2000, rv32, true),
            Some(mepc)
        );
        assert_eq!(
            unwind_program_counter_register(mepc, 0x8000_2000, rv32, false),
            Some(RegisterValue::U32(0x8000_1230))
        );
    }

    #[test]
    fn unwind_constant() {
        let debug_register = CORTEX_M_CORE_REGISTERS.core_registers().next().unwrap();

        let mut callee_frame_registers = DebugRegisters::default();
        callee_frame_registers.0.push(DebugRegister {
            core_register: debug_register,
            dwarf_id: Some(0),
            value: None,
        });
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::PC,
            dwarf_id: Some(15),
            value: Some(RegisterValue::U32(0x0)),
        });

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            debug_register,
            &callee_frame_registers,
            None,
            &mut memory,
            RegisterRule::Constant(0xcafe),
            &ArmV7MExceptionHandler,
        )
        .unwrap();

        assert_eq!(value, DwarfUnwind::Value(Some(RegisterValue::U32(0xcafe))));
    }
}
