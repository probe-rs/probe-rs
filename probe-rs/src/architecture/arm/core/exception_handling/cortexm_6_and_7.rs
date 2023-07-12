use crate::{
    core::{CoreInterface, ExceptionInfo, RegisterRole},
    debug::DebugRegisters,
    Error, RegisterValue,
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
pub(crate) fn get_exception_info<T: CoreInterface>(
    core: &mut T,
    stackframe_registers: &DebugRegisters,
) -> Result<Option<ExceptionInfo>, Error> {
    let frame_return_address: u32 = stackframe_registers
        .get_return_address()
        .ok_or(crate::Error::Other(anyhow::anyhow!(
            "No Return Address register. Please report this as a bug."
        )))?
        .value
        .ok_or(crate::Error::Other(anyhow::anyhow!(
            "No value for Return Address register. Please report this as a bug."
        )))?
        .try_into()?;

    if ExcReturn(frame_return_address).is_exception_flag() == 0xF {
        // This is an exception frame.
        // TODO: probe-rs does not currently do anything with the floating point registers. When support is added, please note that the list of registers to read is different for cores that have the floating point extension.

        Ok(Some(ExceptionInfo {
            description: core.exception_description(stackframe_registers)?,
            calling_frame_registers: core.calling_frame_registers(stackframe_registers)?,
        }))
    } else {
        // This is a normal function return.
        Ok(None)
    }
}

pub(crate) fn calling_frame_registers<T: CoreInterface>(
    core: &mut T,
    stackframe_registers: &crate::debug::DebugRegisters,
) -> Result<crate::debug::DebugRegisters, crate::Error> {
    let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];
    core.read_32(
        stackframe_registers
            .get_register_value_by_role(&crate::core::RegisterRole::StackPointer)?,
        &mut calling_stack_registers,
    )?;
    let mut calling_frame_registers = stackframe_registers.clone();
    for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
        calling_frame_registers
            .get_register_mut_by_role(register_role)
            .ok_or(crate::Error::Other(anyhow::anyhow!(
                "UNWIND: No stack pointer register value. Please report this as a bug."
            )))?
            .value = Some(RegisterValue::U32(calling_stack_registers[i]));
    }
    Ok(calling_frame_registers)
}
