use super::{armv6m_armv7m_shared, ExceptionInfo, ExceptionInterface};
use crate::{DebugError, DebugInfo, DebugRegisters};
use probe_rs::{memory_mapped_bitfield_register, Error, MemoryInterface, MemoryMappedRegister};

memory_mapped_bitfield_register! {
    /// HFSR - HardFault Status Register
    pub struct Hfsr(u32);
    0xE000ED2C, "HFSR",
    impl From;
    debug_event, _: 31;
    escalation_forced, _: 30;
    vector_table_read_fault, _: 1;
}

memory_mapped_bitfield_register! {
    /// CFSR - Configurable Status Register (`UFSR[31:16]`, `BFSR[15:8]`, `MMFSR[7:0]`)
    pub struct Cfsr(u32);
    0xE000ED28, "CFSR",
    impl From;
    /// Aggregate view of the UsageFault bits.
    usage_fault, _: 25,16;
    /// When SDIV or UDIV instruction is used with a divisor of 0, this fault occurs if DIV_0_TRP is enabled in the CCR.
    uf_div_by_zero, _: 25;
    /// Multi-word accesses always fault if not word aligned. Software can configure unaligned word and halfword accesses to fault, by enabling UNALIGN_TRP in the CCR.
    uf_unaligned_access, _: 24;
    /// A coprocessor access error has occurred. This shows that the coprocessor is disabled or not present.
    uf_coprocessor, _: 19;
    /// An integrity check error has occurred on EXC_RETURN.
    uf_integrity_check, _: 18;
    /// Instruction executed with invalid EPSR.T or EPSR.IT field.
    uf_invalid_state, _: 17;
    /// The processor has attempted to execute an undefined instruction. This might be an undefined instruction associated with an enabled coprocessor.
    uf_undefined_instruction, _: 16;
    /// Aggregate view of the BusFault bits.
    bus_fault, _: 15,8;
    /// BFAR has valid contents.
    bf_address_register_valid, _: 15;
    /// A bus fault occurred during FP lazy state preservation.
    bf_fp_lazy_state_preservation, _: 13;
    /// A derived bus fault has occurred on exception entry.
    bf_exception_entry, _: 12;
    /// A derived bus fault has occurred on exception return.
    bf_exception_return, _: 11;
    /// Imprecise data access error has occurred.
    bf_imprecise_data_access_error, _: 10;
    /// Precise data access error has occurred.
    bf_precise_data_access_error, _: 9;
    ///  A bus fault on an instruction prefetch has occurred. The fault is signalled only if the instruction is issued.
    bf_instruction_prefetch, _: 8;
    /// Aggregate view of the MemManage Fault bits.
    mem_manage_fault, _: 7,0;
    ///  MMAR has valid contents.
    mm_address_register_valid, _: 7;
    /// A MemManage fault occurred during FP lazy state preservation.
    mm_fp_lazy_state_preservation, _: 5;
    /// A derived MemManage fault occurred on exception entry.
    mm_exception_entry, _: 4;
    /// A derived MemManage fault occurred on exception return.
    mm_exception_return, _: 3;
    ///  Data access violation. The MMAR shows the data address that the load or store tried to access.
    mm_data_access_violation, _: 1;
    ///  MPU or Execute Never (XN) default memory map access violation on an instruction fetch has occurred.
    mm_instruction_fetch_violation, _: 0;
}

impl Cfsr {
    /// Additional information about a Usage Fault, or Ok(None) if the fault was not a Usage Fault.
    fn usage_fault_description(&self) -> Result<Option<String>, Error> {
        let source = if self.uf_coprocessor() {
            "Coprocessor access error"
        } else if self.uf_div_by_zero() {
            "Division by zero"
        } else if self.uf_integrity_check() {
            "Integrity check error"
        } else if self.uf_invalid_state() {
            "Instruction executed with invalid EPSR.T or EPSR.IT field"
        } else if self.uf_unaligned_access() {
            "Unaligned access"
        } else if self.uf_undefined_instruction() {
            "Undefined instruction"
        } else {
            // Not a UsageFault.
            return Ok(None);
        };
        Ok(Some(format!("UsageFault <Cause: {source}>")))
    }

    /// Additional information about a Bus Fault, or Ok(None) if the fault was not a Bus Fault.
    fn bus_fault_description(
        &self,
        memory: &mut dyn MemoryInterface,
    ) -> Result<Option<String>, Error> {
        let source = if self.bf_exception_entry() {
            "Derived fault on exception entry"
        } else if self.bf_exception_return() {
            "Derived fault on exception return"
        } else if self.bf_fp_lazy_state_preservation() {
            "Fault occurred during FP lazy state preservation"
        } else if self.bf_imprecise_data_access_error() {
            "Imprecise data access error"
        } else if self.bf_instruction_prefetch() {
            "Instruction prefetch"
        } else if self.bf_precise_data_access_error() {
            "Precise data access error"
        } else {
            // Not a BusFault
            return Ok(None);
        };

        Ok(Some(if self.bf_address_register_valid() {
            format!(
                "BusFault <Cause: {source} at location: {:#010x}>",
                memory.read_word_32(Bfar::get_mmio_address())?
            )
        } else {
            format!("BusFault <Cause: {source}>")
        }))
    }

    /// Additional information about a MemManage Fault, or Ok(None) if the fault was not a MemManage Fault.
    fn memory_management_fault_description(
        &self,
        memory: &mut dyn MemoryInterface,
    ) -> Result<Option<String>, Error> {
        let source = if self.mm_data_access_violation() {
            "Data access violation"
        } else if self.mm_exception_entry() {
            "Derived fault on exception entry"
        } else if self.mm_exception_return() {
            "Derived fault on exception return"
        } else if self.mm_fp_lazy_state_preservation() {
            "Fault occurred during FP lazy state preservation"
        } else if self.mm_instruction_fetch_violation() {
            "MPU or Execute Never (XN) default memory map access violation on an instruction fetch"
        } else {
            // Not a MemManage Fault.
            return Ok(None);
        };

        Ok(Some(if self.mm_address_register_valid() {
            format!(
                "MemManage Fault <Cause: {source} at location: {:#010x}>",
                memory.read_word_32(Mmfar::get_mmio_address())?
            )
        } else {
            format!("MemManage Fault <Cause: {source}>")
        }))
    }
}

memory_mapped_bitfield_register! {
    /// MMFAR - MemManage Fault Address Register
    pub struct Mmfar(u32);
    0xE000ED34, "MMFAR",
    impl From;
}

memory_mapped_bitfield_register! {
    /// BFAR - Bus Fault Address Register
    pub struct Bfar(u32);
    0xE000ED38, "BFAR",
    impl From;
}
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
    pub(crate) fn expanded_description(
        &self,
        memory: &mut dyn MemoryInterface,
    ) -> Result<String, Error> {
        match self {
            ExceptionReason::ThreadMode => Ok("<No active exception>".to_string()),
            ExceptionReason::Reset => Ok("Reset".to_string()),
            ExceptionReason::NonMaskableInterrupt => Ok("NMI".to_string()),
            ExceptionReason::HardFault => {
                let hfsr = Hfsr(memory.read_word_32(Hfsr::get_mmio_address())?);
                let description = if hfsr.debug_event() {
                    "Debug fault".to_string()
                } else if hfsr.escalation_forced() {
                    let description = "Escalated";
                    let cfsr = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?);
                    if let Some(source) = cfsr.usage_fault_description()? {
                        format!("{description} {source}")
                    } else if let Some(source) = cfsr.bus_fault_description(memory)? {
                        format!("{description} {source}")
                    } else if let Some(source) = cfsr.memory_management_fault_description(memory)? {
                        format!("{description} {source}")
                    } else {
                        format!("{description} from an unknown source")
                    }
                } else if hfsr.vector_table_read_fault() {
                    "Vector table read fault".to_string()
                } else {
                    "Undeterminable".to_string()
                };
                Ok(format!("HardFault <Cause: {description}>"))
            }
            ExceptionReason::MemoryManagementFault => {
                if let Some(source) = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?)
                    .usage_fault_description()?
                {
                    Ok(source)
                } else {
                    Ok("MemManage Fault <Cause: Unknown>".to_string())
                }
            }
            ExceptionReason::BusFault => {
                if let Some(source) = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?)
                    .bus_fault_description(memory)?
                {
                    Ok(source)
                } else {
                    Ok("BusFault <Cause: Unknown>".to_string())
                }
            }
            ExceptionReason::UsageFault => {
                if let Some(source) = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?)
                    .usage_fault_description()?
                {
                    Ok(source)
                } else {
                    Ok("UsageFault <Cause: Unknown>".to_string())
                }
            }
            ExceptionReason::SVCall => Ok("SVC".to_string()),
            ExceptionReason::DebugMonitor => Ok("DebugMonitor".to_string()),
            ExceptionReason::PendSV => Ok("PendSV".to_string()),
            ExceptionReason::SysTick => Ok("SysTick".to_string()),
            ExceptionReason::ExternalInterrupt(exti) => Ok(format!("External interrupt #{exti}")),
            ExceptionReason::Reserved => {
                Ok("<Reserved by the ISA, and not usable by software>".to_string())
            }
        }
    }

    /// If a precise fault occurs, the PC value in the stack frame will point to the instruction that caused the fault.
    /// This means that the PC value in the stack frame is the address of the faulting instruction.
    ///
    /// For other faults, or interrupts, the PC value in the stack frame will point to the next instruction to be executed.
    ///
    /// See Armv7-M Architecture Reference Manual, section B1.5.6.
    pub(crate) fn is_precise_fault(&self, memory: &mut dyn MemoryInterface) -> Result<bool, Error> {
        let is_precise = match self {
            // Usage fault and memory management fault are always precise.
            ExceptionReason::UsageFault | ExceptionReason::MemoryManagementFault => true,
            ExceptionReason::HardFault | ExceptionReason::BusFault => {
                // Same logic for direct and escalated faults.
                let cfsr = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?);
                cfsr.bf_precise_data_access_error()
                    || cfsr.bf_instruction_prefetch()
                    || cfsr.mem_manage_fault() > 0
                    || cfsr.usage_fault() > 0
            }
            ExceptionReason::DebugMonitor => {
                // This should be true for synchronous exceptions, and false otherwise.
                // TODO: Identify if this debug event was triggered by a vector catch and decode the corresponding FSR. Not a priority for unwinding purposes.
                true
            }
            _ => false,
        };
        Ok(is_precise)
    }
}

/// Exception handling for cores based on the ARMv7-M and ARMv7-EM architectures.
pub struct ArmV7MExceptionHandler;

impl ExceptionInterface for ArmV7MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
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
            armv6m_armv7m_shared::calling_frame_registers(memory_interface, &updated_registers)?;

        if !exception_reason.is_precise_fault(memory_interface)? {
            // PC is always stored on the stack when unwinding an exception,
            // so we know that it exists, and that it has a value
            let pc = updated_registers.get_program_counter_mut().unwrap();

            // If it is not a precise fault, the PC value in the stack frame will point to the next instruction.
            // Subtracing 1 here so that the PC value points to the instruction that caused the fault.
            pc.value.as_mut().unwrap().decrement_address(1)?;
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

    fn exception_description(
        &self,
        raw_exception: u32,
        memory_interface: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        let description =
            ExceptionReason::from(raw_exception).expanded_description(memory_interface)?;
        Ok(description)
    }
}
