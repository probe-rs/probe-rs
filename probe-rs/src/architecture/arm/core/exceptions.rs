pub(crate) mod armv6m {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error,
    };

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv6m::Armv6m<'probe> {
        fn get_exception_info(
            &mut self,
            _stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            todo!("ARMv6-M exception decoding not implemented")
        }
    }
}

pub(crate) mod armv7m {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error, MemoryInterface, RegisterValue,
    };

    /// Decode the exception number.
    #[derive(Debug, Copy, Clone, PartialEq)]
    enum ExceptionReason {
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

    /// When returning from an exception, the processor state registers (architecture specific) will determine which stack and mode to return to.
    #[derive(Debug, Copy, Clone, PartialEq)]
    enum ExceptionReturnContext {
        /// A nested exception was active, return to the handler mode of the nested exception.
        Handler,
        /// Return using a process specific Stack Pointer.
        ProcessStack,
        /// Return using the main Stack Pointer.
        MainStack,
    }

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7m::Armv7m<'probe> {
        fn get_exception_info(
            &mut self,
            stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            // todo!("ARMv7-M exception decoding not implemented");
            // TODO: Currently, this is only tested for ExceptionReturnContext::ToThreadMainStack. This appears to cover all my embassy-rs and RTIC usage scenarios, and I have been unable to trigger the other two cases in normal usage. We need to create custome test cases and an appropriate implementation for the other two cases, i.e., use PSP instead of SP where appropriate)
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

                if let Some(exception_return_context) = match frame_return_address {
                    0xFFFFFFF1 => Some(ExceptionReturnContext::Handler),
                    0xFFFFFFF9 => Some(ExceptionReturnContext::MainStack),
                    0xFFFFFFFD => Some(ExceptionReturnContext::ProcessStack),
                    _ => None,
                } {
                    if let Some(msp_register) = stackframe_registers.get_stack_pointer() {
                        if let Some(Ok(msp_value)) = stackframe_registers
                            .get_register(msp_register.core_register.id)
                            .and_then(|msp| msp.value)
                            .map(<RegisterValue as std::convert::TryInto<u64>>::try_into)
                        {
                            let reason = format!("{:?}", exception_return_context);
                            let mut calling_frame_registers = stackframe_registers.clone();

                            // The MSP register value points to the base of the stack frame that invoked the exception handler. We need to read the stack frame to determine the values of the registers that were stored on the stack.
                            let stored_stack_registers =
                                vec!["R0", "R1", "R2", "R3", "R12", "LR", "PC", "PSR"];
                            let mut calling_stack = vec![0u32; stored_stack_registers.len()];

                            self.read_32(msp_value, &mut calling_stack)?;

                            // We've read the stack frame that invoked the exception handler, so now we need to update the `calling_frame_registers` to match the values we just read.
                            for (i, register_name) in stored_stack_registers.iter().enumerate() {
                                calling_frame_registers.update_register_value_by_name(
                                    register_name,
                                    RegisterValue::U32(calling_stack[i]),
                                )?;
                            }
                            Ok(Some(ExceptionInfo {
                                reason,
                                calling_frame_registers,
                            }))
                        } else {
                            Err(crate::Error::Other(anyhow::anyhow!("UNWIND: The MSP register value {:?} could not be used to determine the next frame base.", msp_register.value)))
                        }
                    } else {
                        Err(crate::Error::Other(anyhow::anyhow!(
                            "UNWIND: No stack pointer register value. Please report this as a bug."
                        )))
                    }
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
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error,
    };

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7a::Armv7a<'probe> {
        fn get_exception_info(
            &mut self,
            _stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            todo!("ARMv7-A exception decoding not implemented")
        }
    }
}

pub(crate) mod armv8a {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error,
    };

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv8a::Armv8a<'probe> {
        fn get_exception_info(
            &mut self,
            _stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            todo!("ARMv8-A exception decoding not implemented")
        }
    }
}

pub(crate) mod armv8m {
    use crate::{
        core::{ExceptionInfo, ExceptionInterface},
        debug::DebugRegisters,
        Error,
    };

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv8m::Armv8m<'probe> {
        fn get_exception_info(
            &mut self,
            _stackframe_registers: &DebugRegisters,
        ) -> Result<Option<ExceptionInfo>, Error> {
            todo!("ARMv8-M exception decoding not implemented")
        }
    }
}
