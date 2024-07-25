use std::ops::ControlFlow;

use crate::{
    core::RegisterRole,
    debug::{
        determine_cfa, get_object_reference, get_unwind_info, stack_frame::StackFrameInfo,
        unwind_register, DebugError, DebugInfo, DebugRegisters, StackFrame,
    },
    Error, MemoryInterface, RegisterValue,
};
use bitfield::bitfield;
use probe_rs_target::InstructionSet;

use super::{ExceptionInfo, ExceptionInterface};

/// Registers which are stored on the stack when an exception occurs.
///
/// - Section B1.5.6, ARMv6-M Architecture Reference Manual. The Armv7-M Architecture Reference Manual
///   has a similar section, where the register list is the same.
/// - We use the terminology `ReturnAddress` instead of `LinkRegister` or `LR`.
/// - The ProgramCounter value depends on the exception type, and for some exceptions, it will
///   be the address of the instruction that caused the exception, while for other exceptions
///   it will be the address of the next instruction after the instruction that caused the exception.
///   See: <https://developer.arm.com/documentation/ddi0403/d/System-Level-Architecture/System-Level-Programmers--Model/ARMv7-M-exception-model/Exception-entry-behavior?lang=en>
pub(crate) static EXCEPTION_STACK_REGISTERS: &[RegisterRole] = &[
    RegisterRole::Core("R0"),
    RegisterRole::Core("R1"),
    RegisterRole::Core("R2"),
    RegisterRole::Core("R3"),
    RegisterRole::Core("R12"),
    RegisterRole::ReturnAddress,
    RegisterRole::ProgramCounter,
    RegisterRole::ProcessorStatus,
];

bitfield! {
    /// The EXC_RETURN value (The value of the link address register) is used to
    /// determine the stack to return to when returning from an exception.
    pub struct ExcReturn(u32);
    impl Debug;
    /// If the value is 0xF, then this is a valid EXC_RETURN value.
    pub is_exception_flag, _: 31, 28;
    /// Defines whether the stack frame for this exception has space allocated for FPU state information.
    /// Bit [4] is 0 if stack space is the extended frame that includes FPU registers.
    /// This impacts the calculation of the new stack pointer.
    pub use_standard_stackframe, _: 4;
    // The final 4 bits are used to identify one of the following 3 behaviours.
    // - 0x1: Return to Handler mode(always uses the Main SP).
    // - 0x9: Return to Thread mode using Main SP.
    // - 0xD: Return to Thread mode using Process SP.
    /// If true, return to Thread mode, else Handler mode.
    pub return_to_thread, _: 3;
    /// If true, return to PSP, else MSP. We can ignore this, because the processor banks the correct value in the SP register.
    pub use_process_stack, _: 2;
    /// When `is_exception_flag` is 0xF, then the last two bits are always 0b01
    pub always_0b01, _: 1,0;
}

bitfield! {
    #[derive(Copy, Clone)]
    /// xPSR - XPSR register is a combined view of APSR, EPSR and IPSR registers.
    /// This is an incomplete/selective mapping of the xPSR register.
    pub struct Xpsr(u32);
    impl Debug;
    pub apsr_n_bit, _: 31;
    pub apsr_z_bit, _: 30;
    pub apsr_c_bit, _: 29;
    pub apsr_v_bit, _: 28;
    /// Indicates if the stack was realigned to 8 byte boundary on exception entry.
    pub stack_was_realigned, _:9;
    pub exception_number, _: 8,0;
}

/// Decode the exception information.
pub(crate) fn exception_details(
    exception_interface: &impl ExceptionInterface,
    memory_interface: &mut dyn MemoryInterface,
    stackframe_registers: &DebugRegisters,
    debug_info: &DebugInfo,
) -> Result<Option<ExceptionInfo>, DebugError> {
    let frame_return_address = get_stack_frame_return_address(stackframe_registers)?;

    if ExcReturn(frame_return_address).is_exception_flag() != 0xF {
        // This is a normal function return / not an exception.
        return Ok(None);
    }

    let raw_exception = exception_interface.raw_exception(stackframe_registers)?;
    let registers = exception_interface.calling_frame_registers(
        memory_interface,
        stackframe_registers,
        raw_exception,
    )?;
    let description = exception_interface.exception_description(raw_exception, memory_interface)?;

    let exception_frame_pc = registers.get_register_value_by_role(&RegisterRole::ProgramCounter)?;

    let mut handler_frame = StackFrame {
        id: get_object_reference(),
        function_name: description.clone(),
        source_location: None,
        registers,
        pc: RegisterValue::U32(exception_frame_pc as u32),
        frame_base: None,
        is_inlined: false,
        local_variables: None,
        canonical_frame_address: None,
    };

    // A fault that is escalated to the priority of a HardFault retains the program counter value of the original fault,
    // so we have to unwind the frame pointer that matches.
    if raw_exception == 3 {
        // A fault that is escalated to the priority of a HardFault retains program counter value of the original fault,
        // So we have to unwind the frame pointer that matches.
        // Determining the frame base may need the CFA (Canonical Frame Address) to be calculated first.
        let mut unwind_context = Box::new(gimli::UnwindContext::new());
        // let exception_interface = exception_handler_for_core(memory_interface.core.core_type());
        // let instruction_set = memory_interface.core.instruction_set().ok();
        let unwind_info = get_unwind_info(
            &mut unwind_context,
            &debug_info.frame_section,
            exception_frame_pc,
        )?;
        handler_frame.canonical_frame_address =
            determine_cfa(&handler_frame.registers, unwind_info)?;
        let Ok((_, functions)) = debug_info.get_function_dies(exception_frame_pc) else {
            handler_frame.function_name = format!("{} : ERROR: While resolving function information for the program counter ({exception_frame_pc:#010x}) that caused the exception.", handler_frame.function_name);
            // Return the available exception info, along with the error information captured in the function_name.
            return Ok(Some(ExceptionInfo {
                raw_exception,
                description,
                handler_frame,
            }));
        };
        if functions.is_empty() {
            handler_frame.function_name = format!("{} : ERROR: No function information for the program counter ({exception_frame_pc:#010x}) that caused the exception.", handler_frame.function_name);
            // Return the available exception info, along with the error information captured in the function_name.
            return Ok(Some(ExceptionInfo {
                raw_exception,
                description,
                handler_frame,
            }));
        }
        handler_frame.frame_base = functions[0].frame_base(
            debug_info,
            memory_interface,
            StackFrameInfo {
                registers: &handler_frame.registers,
                frame_base: None,
                canonical_frame_address: handler_frame.canonical_frame_address,
            },
        )?;
        let callee_frame_registers = handler_frame.registers.clone();
        if let ControlFlow::Break(error) = unwind_register(
            handler_frame
                .registers
                .get_register_mut_by_role(&RegisterRole::FramePointer)?,
            &callee_frame_registers,
            Some(unwind_info),
            handler_frame.canonical_frame_address,
            &mut None,
            memory_interface,
            Some(InstructionSet::Thumb2),
        ) {
            tracing::error!("{:?}", &error);
            handler_frame.function_name =
                format!("{} : ERROR: {error}", handler_frame.function_name);
        };
        // Now we can update the stack pointer also, but
        // first we have to determine the size of the exception data on the stack.
        // See <https://developer.arm.com/documentation/ddi0403/d/System-Level-Architecture/System-Level-Programmers--Model/ARMv7-M-exception-model/Exception-entry-behavior?lang=en>
        let frame_size = if ExcReturn(frame_return_address).use_standard_stackframe() {
            // This is a standard exception frame.
            0x20usize
        } else {
            // This is an extended frame that includes FPU registers.
            0x68
        };
        // // Now we can update the registers with the new stack pointer.
        let sp = handler_frame
            .registers
            .get_register_mut_by_role(&RegisterRole::StackPointer)?;
        if let Some(sp_value) = sp.value.as_mut() {
            sp_value.increment_address(frame_size)?;
        }
    }

    Ok(Some(ExceptionInfo {
        raw_exception,
        description,
        handler_frame,
    }))
}

fn get_stack_frame_return_address(stackframe_registers: &DebugRegisters) -> Result<u32, Error> {
    let frame_return_address: u32 = stackframe_registers
        .get_return_address()
        .ok_or_else(|| {
            Error::Register("No Return Address register. Please report this as a bug.".to_string())
        })?
        .value
        .ok_or_else(|| {
            Error::Register(
                "No value for Return Address register. Please report this as a bug.".to_string(),
            )
        })?
        .try_into()?;
    Ok(frame_return_address)
}

pub(crate) fn raw_exception(
    stackframe_registers: &crate::debug::DebugRegisters,
) -> Result<u32, Error> {
    // Load the provided xPSR register as a bitfield.
    let mut exception_number = Xpsr(
        stackframe_registers.get_register_value_by_role(&RegisterRole::ProcessorStatus)? as u32,
    )
    .exception_number();
    if exception_number == 0
        && stackframe_registers.get_register_value_by_role(&RegisterRole::ReturnAddress)?
            == 0xFFFF_FFFF
    {
        // Although the exception number is 0, for the purposes of unwind, this is treated as a reset exception.
        // This is because of the timing when the processor resets the exception number from 1 to 0, versus
        // when the LR register (EXC_RETURN) is set to 0xFFFFFFFF.
        // The processor timing biases towards exception return behavior, while we are
        // only interested in capturing the exception frame information for unwind purposes.
        // For more information, see the section, "The special-purpose program status registers, xPSR"
        // and "Reset Behaviour" in the ARMv7-m Architecture Reference Manual,
        // - "On reset, the processor is in Thread mode and ...
        //   - ... the Exception Number field of the IPSR is cleared to 0. As a result, the value 1, the exception number for reset,
        //    is a transitory value, that software cannot see as a valid IPSR Exception Number."
        //   - The LR register value is set to 0xFFFFFFFF (The reset value)
        exception_number = 1;
    }

    Ok(exception_number)
}

/// The calling frame registers are a predefined set of registers that are stored on the stack when an exception occurs.
/// The registers are stored in that list in the order they are defined in the `EXCEPTION_STACK_REGISTERS` array.
/// This function will read the values of the registers from the stack and update the passed `stackframe_registers` with the new values.
// TODO: probe-rs does not currently do anything with the floating point registers. When support is added, please note that the list of registers to read is different for cores that have the floating point extension.
pub(crate) fn calling_frame_registers(
    memory: &mut dyn MemoryInterface,
    stackframe_registers: &crate::debug::DebugRegisters,
    _raw_exception: u32,
) -> Result<crate::debug::DebugRegisters, crate::Error> {
    let exception_context_address: u32 =
        stackframe_registers.get_register_value_by_role(&RegisterRole::StackPointer)? as u32;

    // Get the values of the registers pushed onto the stack.
    let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];
    memory.read_32(
        (exception_context_address).into(),
        &mut calling_stack_registers,
    )?;

    let mut calling_frame_registers = stackframe_registers.clone();
    for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
        calling_frame_registers
            .get_register_mut_by_role(register_role)?
            .value = Some(RegisterValue::U32(calling_stack_registers[i]));
    }

    Ok(calling_frame_registers)
}
