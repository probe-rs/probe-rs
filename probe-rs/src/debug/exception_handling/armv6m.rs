use crate::{
    debug::{DebugError, DebugInfo, DebugRegisters},
    Error, MemoryInterface,
};

use super::{armv6m_armv7m_shared, ExceptionInfo, ExceptionInterface};

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
    pub(crate) fn is_precise_fault(
        &self,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<bool, Error> {
        Ok(match self {
            ExceptionReason::HardFault => {
                // This should be true for synchronous exceptions, and false otherwise.
                // TODO: Figure out how to differentiate that on ARMv6-M.
                true
            }
            _ => false,
        })
    }
}

/// Exception handling for cores based on the ARMv6-M architecture.
pub struct ArmV6MExceptionHandler;

impl ExceptionInterface for ArmV6MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        armv6m_armv7m_shared::exception_details(
            self,
            memory_interface,
            stackframe_registers,
            debug_info,
        )
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
        raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, DebugError> {
        let mut updated_registers = stackframe_registers.clone();

        // Identify the correct location for the exception context. This is different between Armv6-M and Armv7-M.
        let exception_reason = ExceptionReason::from(raw_exception);
        if exception_reason.is_precise_fault(memory_interface)? {
            let exception_context_address =
                updated_registers.get_register_mut_by_role(&crate::RegisterRole::StackPointer)?;
            if let Some(sp_value) = exception_context_address.value.as_mut() {
                sp_value.increment_address(0x8)?;
            }
        }

        updated_registers = armv6m_armv7m_shared::calling_frame_registers(
            memory_interface,
            &updated_registers,
            raw_exception,
        )?;

        Ok(updated_registers)
    }

    fn raw_exception(
        &self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, DebugError> {
        let value = armv6m_armv7m_shared::raw_exception(stackframe_registers)?;
        Ok(value)
    }

    fn exception_description(
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
    use crate::{
        architecture::arm::core::registers::cortex_m::{RA, XPSR},
        debug::exception_handling::ExceptionInterface,
        debug::{DebugRegister, DebugRegisters},
        test::MockMemory,
        RegisterValue,
    };

    #[test]
    fn exception_handler_reset_exception() {
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
            .unwrap();

        assert_eq!(description, "Reset")
    }

    #[test]
    fn exception_handler_no_exception_description() {
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
            .unwrap();

        assert_eq!(description, "<No active exception>")
    }
}
