use crate::MemoryInterface;

use super::armv6m_armv7m_shared::Xpsr;

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

pub fn exception_description(
    _memory_interface: &mut dyn MemoryInterface,
    stackframe_registers: &crate::debug::DebugRegisters,
) -> Result<String, crate::Error> {
    // Load the provided xPSR register as a bitfield.
    let exception_number = Xpsr(
        stackframe_registers
            .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
            as u32,
    )
    .exception_number();

    // TODO: Some ARMv6-M cores (e.g. the Cortex-M0) do not have HFSR and CFGR registers, so we cannot
    //       determine the cause of the hard fault. We should add a check for this, and return a more
    //       helpful error message in this case (I'm not sure this is possible).
    //       Until then, this will return a generic error message for all hard faults on this architecture.
    Ok(format!("{:?}", ExceptionReason::from(exception_number)))
}
