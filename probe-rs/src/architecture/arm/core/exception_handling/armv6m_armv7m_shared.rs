use crate::{
    core::{ExceptionInfo, ExceptionInterface, RegisterRole},
    debug::DebugRegisters,
    Error, MemoryInterface, RegisterValue,
};
use bitfield::bitfield;

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
    /// Defines whether the stack frame for this exception has space allocated for FPU state information. Bit [4] is 0 if stack space is the exended frame that includes FPU registes.
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
    adapter: &mut (impl MemoryInterface + ExceptionInterface),
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

        Ok(Some(ExceptionInfo {
            description: adapter.exception_description(stackframe_registers)?,
            calling_frame_registers: adapter.calling_frame_registers(stackframe_registers)?,
        }))
    } else {
        // This is a normal function return.
        Ok(None)
    }
}

/// The calling frame registers are a predefined set of registers that are stored on the stack when an exception occurs.
/// The registers are stored in that list in the order they are defined in the `EXCEPTION_STACK_REGISTERS` array.
/// This function will read the values of the registers from the stack and update the passed `stackframe_registers` with the new values.
// TODO: probe-rs does not currently do anything with the floating point registers. When support is added, please note that the list of registers to read is different for cores that have the floating point extension.
pub(crate) fn calling_frame_registers(
    adapter: &mut impl MemoryInterface,
    stackframe_registers: &crate::debug::DebugRegisters,
) -> Result<crate::debug::DebugRegisters, crate::Error> {
    let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];
    adapter.read_32(
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
    Ok(calling_frame_registers)
}
