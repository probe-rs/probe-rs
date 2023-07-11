/// Where applicable, this defines shared logic for implementing exception handling accross the various ARM [`crate::CoreType`]'s.
pub(crate) mod cortexm {
    use crate::core::RegisterRole;

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

    /// Decode the exception number.
    #[derive(Debug, Copy, Clone, PartialEq)]
    pub(crate) enum ExceptionReason {
        /// No exception is active.
        ThreadMode,
        /// A reset has been triggered.
        Reset,
        /// A non-maskable interrupt has been triggered.
        NonMaskableInterrupt,
        /// A hard fault has been triggered.
        HardFault,
        /// A memory management fault has been triggered.
        MemoryManagementFault,
        /// A bus fault has been triggered.
        BusFault,
        /// A usage fault has been triggered.
        UsageFault,
        /// A SuperVisor call has been triggered.
        SVCall,
        /// A debug monitor fault has been triggered.
        DebugMonitor,
        /// A non-maskable interrupt has been triggered.
        PendSV,
        /// A non-maskable interrupt has been triggered.
        SysTick,
        /// A non-maskable interrupt has been triggered.
        ExternalInterrupt(u32),
        /// Reserved by the ISA, and not usable by software.
        Reserved,
    }

    impl From<u32> for ExceptionReason {
        fn from(exception: u32) -> Self {
            match exception {
                0 => ExceptionReason::ThreadMode,
                1 => ExceptionReason::Reset,
                7..=10 | 13 => ExceptionReason::Reserved,
                2 => ExceptionReason::NonMaskableInterrupt,
                3 => ExceptionReason::HardFault,
                4 => ExceptionReason::MemoryManagementFault,
                5 => ExceptionReason::BusFault,
                6 => ExceptionReason::UsageFault,
                11 => ExceptionReason::SVCall,
                12 => ExceptionReason::DebugMonitor,
                14 => ExceptionReason::PendSV,
                15 => ExceptionReason::SysTick,
                // TODO: Does it make sense to try to interpret the RHS boundary of valid ISR numbers?
                16.. => ExceptionReason::ExternalInterrupt(exception - 16),
            }
        }
    }

    /// When returning from an exception, the processor state registers (architecture specific) will determine which stack.
    #[derive(Debug, Copy, Clone, PartialEq)]
    pub(crate) enum ExceptionReturnContext {
        /// Triggered from another exception, return from the handler to the main stack.
        HandlerToMain,
        /// Triggered from an active thread, return to a process specific Stack Pointer.
        ThreadToProcess,
        /// Triggered from an active thread, return to a to the main Stack Pointer.
        ThreadToMain,
    }

    impl ExceptionReturnContext {
        /// Unpack the exception return context from the EXC_RETURN value (The value of the link address register).
        /// Note: Even though probe-rs does not use the FPU registers explicitly, we need to take into account the
        /// different EXC_RETURN values for FPU supported cores.
        pub(crate) fn from_exc_return(
            frame_return_address: u32,
            fpu_supported: bool,
        ) -> Option<Self> {
            if fpu_supported {
                match frame_return_address {
                    0xFFFFFFE1 | 0xFFFFFFF1 => Some(ExceptionReturnContext::HandlerToMain),
                    0xFFFFFFE9 | 0xFFFFFFF9 => Some(ExceptionReturnContext::ThreadToMain),
                    0xFFFFFFED | 0xFFFFFFFD => Some(ExceptionReturnContext::ThreadToProcess),
                    _ => None,
                }
            } else {
                match frame_return_address {
                    0xFFFFFFF1 => Some(ExceptionReturnContext::HandlerToMain),
                    0xFFFFFFF9 => Some(ExceptionReturnContext::ThreadToMain),
                    0xFFFFFFFD => Some(ExceptionReturnContext::ThreadToProcess),
                    _ => None,
                }
            }
        }
    }
}

pub(crate) mod armv6m {
    use crate::core::ExceptionInterface;

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv6m::Armv6m<'probe> {}
}
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        CoreInterface, Error, MemoryInterface, RegisterValue,
    };

    use super::cortexm::{ExceptionReturnContext, EXCEPTION_STACK_REGISTERS};

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7m::Armv7m<'probe> {
        /// Decode the exception information. Largely based on [ARM documentation here](https://developer.arm.com/documentation/ddi0403/d/System-Level-Architecture/System-Level-Programmers--Model/ARMv7-M-exception-model/Exception-return-behavior?lang=en).
        fn get_exception_info(
            &mut self,
            stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            if let Some(return_register_value) = stackframe_registers
                .get_return_address()
                .and_then(|return_register| return_register.value)
            {
                let frame_return_address: u32= return_register_value.try_into().map_err(|error| {
                    crate::Error::Other(anyhow::anyhow!(
                        "UNWIND: Failed to convert LR register value to address: {:?}. Please report this as a bug.",
                        error
                    ))
                })?;

                if let Some(exception_return_context) = ExceptionReturnContext::from_exc_return(
                    frame_return_address,
                    self.fpu_support()?,
                ) {
                    let calling_stack_base_address: u64 = match exception_return_context {
                        ExceptionReturnContext::HandlerToMain
                        | ExceptionReturnContext::ThreadToMain => stackframe_registers
                            .get_register_by_role(&crate::core::RegisterRole::MainStackPointer)
                            .ok_or(crate::Error::Other(anyhow::anyhow!(
                                "UNWIND: No main stack pointer register. Please report this as a bug."
                            )))?
                            .value
                            .ok_or(crate::Error::Other(anyhow::anyhow!(
                                "UNWIND: No main stack pointer register value. Please report this as a bug."
                            )))?
                            .try_into()?,
                        ExceptionReturnContext::ThreadToProcess => stackframe_registers
                            .get_register_by_role(
                                &crate::core::RegisterRole::ProcessStackPointer,
                            )
                            .ok_or(crate::Error::Other(anyhow::anyhow!(
                                "UNWIND: No process stack pointer register. Please report this as a bug."
                            )))?
                            .value
                            .ok_or(crate::Error::Other(anyhow::anyhow!(
                                "UNWIND: No process stack pointer register value. Please report this as a bug."
                            )))?
                            .try_into()?,
                        };

                    // TODO: probe-rs does not currently do anything with the floating point registers. When support is added, please note that the list of registers to read is different for cores that have the floating point extension.
                    let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];

                    self.read_32(calling_stack_base_address, &mut calling_stack_registers)?;

                    let reason = format!(
                        "{:?} from {calling_stack_base_address:#010x}",
                        exception_return_context
                    );
                    let mut calling_frame_registers = stackframe_registers.clone();

                    // We've read the stack frame that invoked the exception handler, so now we need to update the `calling_frame_registers` to match the values we just read.
                    for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
                        calling_frame_registers
                                    .get_register_mut_by_role(register_role)
                                    .ok_or(crate::Error::Other(anyhow::anyhow!("UNWIND: No stack pointer register value. Please report this as a bug.")))?
                                    .value = Some(RegisterValue::U32(calling_stack_registers[i]));
                    }
                    Ok(Some(ExceptionInfo {
                        reason,
                        calling_frame_registers,
                    }))
                } else {
                    // This is a normal function return, not an exception return.
                    Ok(None)
                }
            } else {
                Err(crate::Error::Other(anyhow::anyhow!(
                    "UNWIND: No LR register value. Please report this as a bug."
                )))
            }
        }
    }
}

pub(crate) mod armv7a {
    use crate::core::ExceptionInterface;

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7a::Armv7a<'probe> {}
}

pub(crate) mod armv8a {
    use crate::core::ExceptionInterface;

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv8a::Armv8a<'probe> {}
}

pub(crate) mod armv8m {
    use crate::core::ExceptionInterface;

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv8m::Armv8m<'probe> {}
}
