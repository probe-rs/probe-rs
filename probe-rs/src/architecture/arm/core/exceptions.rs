/// Where applicable, this defines shared logic for implementing exception handling accross the various ARM [`crate::CoreType`]'s.
pub(crate) mod cortexm {
    use crate::core::RegisterRole;
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
                16.. => ExceptionReason::ExternalInterrupt(exception - 16),
            }
        }
    }

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
        pub apsr_q_bit, _: 27;
        pub ipsr_exception_number, _: 8,0;
    }

    impl Xpsr {
        /// Decode the exception number.
        pub(crate) fn exception_reason(&self) -> ExceptionReason {
            self.ipsr_exception_number().into()
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
        Error, MemoryInterface, RegisterValue,
    };

    use super::cortexm::{ExcReturn, Xpsr, EXCEPTION_STACK_REGISTERS};

    impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7m::Armv7m<'probe> {
        /// Decode the exception information. Largely based on [ARM documentation here](https://developer.arm.com/documentation/ddi0403/d/System-Level-Architecture/System-Level-Programmers--Model/ARMv7-M-exception-model/Exception-return-behavior?lang=en).
        fn get_exception_info(
            &mut self,
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
                let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];

                self.read_32(
                    stackframe_registers
                        .get_register_value_by_role(&crate::core::RegisterRole::StackPointer)?,
                    &mut calling_stack_registers,
                )?;

                // Load the provided xPSR register as a bitfield.
                let xpsr_register = Xpsr(
                    stackframe_registers
                        .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
                        as u32,
                );

                let reason = format!("{:?}", xpsr_register.exception_reason());

                let mut calling_frame_registers = stackframe_registers.clone();

                // We've read the stack frame that invoked the exception handler, so now we need to update the `calling_frame_registers` to match the values we just read.
                for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
                    calling_frame_registers
                        .get_register_mut_by_role(register_role)
                        .ok_or(crate::Error::Other(anyhow::anyhow!(
                            "UNWIND: No stack pointer register value. Please report this as a bug."
                        )))?
                        .value = Some(RegisterValue::U32(calling_stack_registers[i]));
                }
                Ok(Some(ExceptionInfo {
                    reason,
                    calling_frame_registers,
                }))
            } else {
                // This is a normal function return.
                Ok(None)
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
