use crate::{
    core::{ExceptionInfo, ExceptionInterface},
    debug::DebugRegisters,
    CoreInterface, Error,
};

use super::cortexm_6_and_7::{self, calling_frame_registers, Xpsr};

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
            2 => ExceptionReason::NonMaskableInterrupt,
            3 => ExceptionReason::HardFault,
            4 => ExceptionReason::MemoryManagementFault,
            5 => ExceptionReason::BusFault,
            6 => ExceptionReason::UsageFault,
            7..=10 | 13 => ExceptionReason::Reserved,
            11 => ExceptionReason::SVCall,
            12 => ExceptionReason::DebugMonitor,
            14 => ExceptionReason::PendSV,
            15 => ExceptionReason::SysTick,
            16.. => ExceptionReason::ExternalInterrupt(exception - 16),
        }
    }
}

impl ExceptionReason {
    /// Expands the exception reason, by providing additional information about the exception from the
    /// HFSR and CFSR registers.
    fn expanded_description<T: CoreInterface>(&self, core: &mut T) -> Result<String, Error> {
        match self {
            ExceptionReason::ThreadMode => Ok("No active exception.".to_string()),
            ExceptionReason::Reset => Ok("".to_string()),
            ExceptionReason::NonMaskableInterrupt => todo!(),
            ExceptionReason::HardFault => todo!(),
            ExceptionReason::MemoryManagementFault => todo!(),
            ExceptionReason::BusFault => todo!(),
            ExceptionReason::UsageFault => todo!(),
            ExceptionReason::SVCall => todo!(),
            ExceptionReason::DebugMonitor => todo!(),
            ExceptionReason::PendSV => todo!(),
            ExceptionReason::SysTick => Ok("".to_string()),
            ExceptionReason::ExternalInterrupt(exti) => Ok(format!("External interrupt #{exti}.")),
            ExceptionReason::Reserved => {
                Ok("Reserved by the ISA, and not usable by software.".to_string())
            }
        }
    }
}

impl<'probe> ExceptionInterface for crate::architecture::arm::core::armv7m::Armv7m<'probe> {
    fn calling_frame_registers(
        &mut self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        calling_frame_registers(self, stackframe_registers)
    }

    fn exception_description(
        &mut self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        // Load the provided xPSR register as a bitfield.
        let exception_number = Xpsr(
            stackframe_registers
                .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
                as u32,
        )
        .exception_number();

        Ok(format!(
            "{:?}",
            ExceptionReason::from(exception_number).expanded_description(self)?
        ))
    }

    fn get_exception_info(
        &mut self,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        cortexm_6_and_7::get_exception_info(self, stackframe_registers)
    }
}
