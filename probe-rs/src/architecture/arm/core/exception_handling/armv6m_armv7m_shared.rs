use crate::{
    core::{ExceptionInfo, ExceptionInterface, RegisterRole},
    debug::DebugRegisters,
    Error, MemoryInterface, RegisterValue,
};
use bitfield::bitfield;
use num_traits::Zero;

/// Registers which are stored on the stack when an exception occurs.
///
/// - Section B1.5.6, ARMv6-M Architecture Reference Manual
///
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
    /// If the value is 0xF, then this is a valid EXC_RETURN value.
    pub is_exception_flag, _: 31, 28;
    /// Defines whether the stack frame for this exception has space allocated for FPU state information. Bit [4] is 0 if stack space is the extended frame that includes FPU registers.
    pub use_standard_stackframe, _: 4;
    /// Identifies one of the following 3 behaviours.
    /// - 0x1: Return to Handler mode(always uses the Main SP).
    /// - 0x9: Return to Thread mode using Main SP.
    /// - 0xD: Return to Thread mode using Process SP.
    pub exception_behaviour, _: 3,0;
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
    pub exception_number, _: 8,0;
}

/// Decode the exception information.
pub(crate) fn exception_details(
    exception_interface: &dyn ExceptionInterface,
    memory_interface: &mut dyn MemoryInterface,
    stackframe_registers: &DebugRegisters,
) -> Result<Option<ExceptionInfo>, Error> {
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

    if ExcReturn(frame_return_address).is_exception_flag() == 0xF {
        // This is an exception frame.

        let raw_exception = exception_interface.raw_exception(stackframe_registers)?;

        Ok(Some(ExceptionInfo {
            raw_exception,
            description: exception_interface
                .exception_description(raw_exception, memory_interface)?,
            calling_frame_registers: exception_interface
                .calling_frame_registers(memory_interface, stackframe_registers)?,
        }))
    } else {
        // This is a normal function return.
        Ok(None)
    }
}

pub(crate) fn raw_exception(
    stackframe_registers: &crate::debug::DebugRegisters,
) -> Result<u32, Error> {
    // Load the provided xPSR register as a bitfield.
    let mut exception_number = Xpsr(
        stackframe_registers
            .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
            as u32,
    )
    .exception_number();
    if exception_number.is_zero()
        && stackframe_registers
            .get_register_value_by_role(&crate::core::RegisterRole::ReturnAddress)?
            == 0xFFFF_FFFF
    {
        // Although the exception number is 0, for the purposes of unwind, this treated as a reset exception.
        // Based on the sections, "The special-purpose program status registers, xPSR"
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
) -> Result<crate::debug::DebugRegisters, crate::Error> {
    let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];

    memory.read_32(
        stackframe_registers
            .get_register_value_by_role(&crate::core::RegisterRole::StackPointer)?,
        &mut calling_stack_registers,
    )?;

    let mut calling_frame_registers = stackframe_registers.clone();
    for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
        calling_frame_registers
            .get_register_mut_by_role(register_role)?
            .value = Some(RegisterValue::U32(calling_stack_registers[i]));
    }

    // Adjust stack pointer
    let sp = calling_frame_registers
        .get_register_mut_by_role(&crate::core::RegisterRole::StackPointer)?;

    if let Some(sp_value) = &mut sp.value {
        sp_value
            .increment_address(4 * EXCEPTION_STACK_REGISTERS.len())
            .unwrap();
    }

    Ok(calling_frame_registers)
}
