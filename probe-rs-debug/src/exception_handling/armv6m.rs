use crate::{DebugError, DebugInfo, DebugRegisters};
use probe_rs::MemoryInterface;

use super::{ExceptionInfo, ExceptionInterface, armv6m_armv7m_shared};

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
    /// A SuperVisor call has been triggered.
    SVCall,
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
            2 => ExceptionReason::NonMaskableInterrupt,
            3 => ExceptionReason::HardFault,
            4..=10 | 12 | 13 => ExceptionReason::Reserved,
            11 => ExceptionReason::SVCall,
            14 => ExceptionReason::PendSV,
            15 => ExceptionReason::SysTick,
            16.. => ExceptionReason::ExternalInterrupt(exception - 16),
        }
    }
}

impl ExceptionReason {
    /// Expands the exception reason, by providing additional information about the exception from the
    /// HFSR and CFSR registers.
    pub(crate) fn expanded_description(&self) -> String {
        match self {
            ExceptionReason::ThreadMode => "<No active exception>".to_string(),
            ExceptionReason::Reset => "Reset".to_string(),
            ExceptionReason::NonMaskableInterrupt => "NMI".to_string(),
            ExceptionReason::HardFault => "HardFault".to_string(),
            ExceptionReason::SVCall => "SVC".to_string(),
            ExceptionReason::PendSV => "PendSV".to_string(),
            ExceptionReason::SysTick => "SysTick".to_string(),
            ExceptionReason::ExternalInterrupt(exti) => format!("External interrupt #{exti}"),
            ExceptionReason::Reserved => {
                "<Reserved by the ISA, and not usable by software>".to_string()
            }
        }
    }

    /// Determines how the exception return address should be offset when unwinding the stack.
    /// See Armv6-M Architecture Reference Manual, section B1.5.6.
    pub(crate) fn is_precise_fault(&self) -> bool {
        match self {
            ExceptionReason::HardFault => {
                // This should be true for synchronous exceptions, and false otherwise.
                // TODO: Figure out how to differentiate that on ARMv6-M.
                true
            }
            _ => false,
        }
    }
}

/// Exception handling for cores based on the ARMv6-M architecture.
pub struct ArmV6MExceptionHandler;

#[async_trait::async_trait(?Send)]
impl ExceptionInterface for ArmV6MExceptionHandler {
    async fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers).await
    }

    async fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::DebugRegisters,
        raw_exception: u32,
    ) -> Result<crate::DebugRegisters, DebugError> {
        let exception_reason = ExceptionReason::from(raw_exception);

        // This shouldn't be called for Reset, because for Reset, no registers
        // are stored on the stack.
        if exception_reason == ExceptionReason::Reset {
            return Err(DebugError::Other(
                "Unwinding over Reset is not possible.".to_string(),
            ));
        }

        let mut updated_registers = stackframe_registers.clone();

        updated_registers =
            armv6m_armv7m_shared::calling_frame_registers(memory_interface, &updated_registers)
                .await?;

        if !exception_reason.is_precise_fault() {
            // PC is always stored on the stack when unwinding an exception,
            // so we know that it exists, and that it has a value
            let pc = updated_registers.get_program_counter_mut().unwrap();

            // If it is not a precise fault, the PC value in the stack frame will point to the next instruction.
            // Subtracing 1 here so that the PC value points to the instruction that caused the fault.
            if pc.value.as_mut().unwrap().decrement_address(1).is_err() {
                // Ignore errors here, better to continue in the unlikely case that we encounter PC = 0x0.
                // It might be that in that case, the actual exception *was* a precise fault.
                tracing::debug!(
                    "UNWIND: Failed to reproduce caller program counter, using PC unchanged."
                );
            }
        }

        Ok(updated_registers)
    }

    fn raw_exception(
        &self,
        stackframe_registers: &crate::DebugRegisters,
    ) -> Result<u32, DebugError> {
        let value = armv6m_armv7m_shared::raw_exception(stackframe_registers)?;
        Ok(value)
    }

    async fn exception_description(
        &self,
        raw_exception: u32,
        _memory_interface: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        // TODO: Some ARMv6-M cores (e.g. the Cortex-M0) do not have HFSR and CFGR registers, so we cannot
        //       determine the cause of the hard fault. We should add a check for this, and return a more
        //       helpful error message in this case (I'm not sure this is possible).
        //       Until then, this will return a generic error message for all hard faults on this architecture.
        Ok(ExceptionReason::from(raw_exception).expanded_description())
    }
}

#[cfg(test)]
mod test {
    use pretty_assertions::assert_eq;

    use super::ArmV6MExceptionHandler;
    use probe_rs::{
        RegisterValue,
        architecture::arm::core::registers::cortex_m::{RA, XPSR},
        test::MockMemory,
    };

    use crate::exception_handling::ExceptionInterface;
    use crate::{DebugRegister, DebugRegisters};

    #[pollster::test]
    async fn exception_handler_reset_exception() {
        let handler = ArmV6MExceptionHandler {};

        let mut memory = MockMemory::new();
        let mut registers = DebugRegisters(vec![]);

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &XPSR,
            value: Some(RegisterValue::U32(0)),
        });
        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &RA,
            value: Some(RegisterValue::U32(0xFFFF_FFFF)),
        });

        let raw_exception = handler.raw_exception(&registers).unwrap();

        let description = handler
            .exception_description(raw_exception, &mut memory)
            .await
            .unwrap();

        assert_eq!(description, "Reset")
    }

    #[pollster::test]
    async fn exception_handler_no_exception_description() {
        let handler = ArmV6MExceptionHandler {};

        let mut memory = MockMemory::new();
        let mut registers = DebugRegisters(vec![]);

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &XPSR,
            value: Some(RegisterValue::U32(0)),
        });
        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &RA,
            value: Some(RegisterValue::U32(0)),
        });

        let raw_exception = handler.raw_exception(&registers).unwrap();

        let description = handler
            .exception_description(raw_exception, &mut memory)
            .await
            .unwrap();

        assert_eq!(description, "<No active exception>")
    }
}
