use crate::{DebugError, DebugInfo, DebugRegisters, StackFrame, get_object_reference};
use bitfield::bitfield;
use probe_rs::{
    Error, MemoryInterface, MemoryMappedRegister, RegisterRole, RegisterValue,
    memory_mapped_bitfield_register,
};

use super::{
    ExceptionInfo, ExceptionInterface,
    armv6m_armv7m_shared::{EXCEPTION_STACK_REGISTERS, Xpsr},
};

bitfield! {
    /// The EXC_RETURN value (The value of the link address register) is used to
    /// determine the stack to return to when returning from an exception.
    struct ExcReturn(u32);
    /// If the value is 0xFF, then this is a valid EXC_RETURN value.
    is_exception_flag, _: 31, 24;
    /// Indicates whether to restore registers from the secure stack or the unsecure stack
    use_secure_stack, _:6;
    /// Indicates whether the default stacking rules apply or the callee registers are already on the stack
    use_default_register_stacking, _:5;
    /// Defines whether the stack frame for this exception has space allocated for FPU state information. Bit [4] is 0 if stack space is the extended frame that includes FPU registers.
    use_standard_stackframe, _: 4;
    /// Indicates whether the return mode is Handler (0) or Thread (1)
    mode, _: 3;
    /// Indicates which stack pointer the exception frame resides on
    stack_pointer_selection, _: 2;
    /// Indicates whether the security domain the exception was taken to was non-secure (0) or secure (1)
    exception_secure, _: 0;
}

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
        Ok(Some(format!("UsageFault ({source})")))
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
                "BusFault ({source}) at location: {:#010x}",
                memory.read_word_32(Bfar::get_mmio_address())?
            )
        } else {
            format!("BusFault ({source})")
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
                "MemManage Fault({source}) at location: {:#010x}",
                memory.read_word_32(Mmfar::get_mmio_address())?
            )
        } else {
            format!("MemManage Fault({source})")
        }))
    }
}

memory_mapped_bitfield_register! {
    /// SFSR - Secure Fault Status Register
    pub struct Sfsr(u32);
    0xE000EDE4, "SFSR",
    impl From;
    /// Sticky flag indicating that an error occurred during lazy Floating-point state preservation activation or deactivation.
    lazy_state_error, _: 7;
    /// This bit is set when the SFAR register contains a valid value.
    secure_fault_address_valid, _: 6;
    /// Sticky flag indicating that an SAU or IDAU violation occurred during the lazy Floating-point state preservation.
    lazy_state_preservation_error, _: 5;
    /// Sticky flag indicating that an exception was raised due to a branch that was not flagged as being domain crossing causing a transition from Secure to Non-secure memory.
    invalid_transition, _: 4;
    /// Sticky flag indicating that an attempt was made to access parts of the address space that are marked as Secure with NS-Req for the transaction set to Non-secure.
    attribution_unit_violation, _: 3;
    /// This can be caused by EXC_RETURN.DCRS being set to 0 when returning from an exception in the Non-secure state, or by EXC_RETURN.ES being set to 1 when returning from an exception in the Non-secure state.
    invalid_exception_return, _: 2;
    /// This bit is set if the integrity signature in an exception stack frame is found to be invalid during the unstacking operation.
    invalid_integrity_signature, _: 1;
    /// This bit is set if there is an invalid attempt to enter Secure state.
    invalid_entry_point, _: 0;
}

impl Sfsr {
    /// Additional information about a Secure Fault, or Ok(None) if the fault was not a Secure Fault.
    ///
    /// This function will access the memory interface to determine the address of the fault,
    /// if necessary.
    fn secure_fault_description(
        &self,
        memory: &mut dyn MemoryInterface,
    ) -> Result<Option<String>, Error> {
        let source = if self.lazy_state_error() {
            "Fault occurred during lazy state activation or deactivation"
        } else if self.lazy_state_preservation_error() {
            "Fault occurred during FP lazy state preservation"
        } else if self.invalid_transition() {
            "Invalid transition error"
        } else if self.attribution_unit_violation() {
            "Attribution unit violation error"
        } else if self.invalid_exception_return() {
            "Invalid exception return error"
        } else if self.invalid_integrity_signature() {
            "Invalid integrity signature error"
        } else if self.invalid_entry_point() {
            "Invalid entry point error"
        } else {
            // Not a SecureFault
            return Ok(None);
        };

        Ok(Some(if self.secure_fault_address_valid() {
            format!(
                "SecureFault ({source}) at location: {:#010x}",
                memory.read_word_32(Sfar::get_mmio_address())?
            )
        } else {
            format!("SecureFault ({source})")
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

memory_mapped_bitfield_register! {
    /// SFAR - Secure Fault Address Register
    pub struct Sfar(u32);
    0xE000EDE8, "SFAR",
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
    /// A secure fault has been triggered.
    SecureFault,
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
            7 => ExceptionReason::SecureFault,
            8..=10 | 13 => ExceptionReason::Reserved,
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
    /// HFSR, CFSR, and SFSR registers.
    fn expanded_description(&self, memory: &mut dyn MemoryInterface) -> Result<String, DebugError> {
        match self {
            ExceptionReason::ThreadMode => Ok("<No active exception>".to_string()),
            ExceptionReason::Reset => Ok("Reset".to_string()),
            ExceptionReason::NonMaskableInterrupt => Ok("NMI".to_string()),
            ExceptionReason::HardFault => {
                let hfsr = Hfsr(memory.read_word_32(Hfsr::get_mmio_address())?);
                let description = if hfsr.debug_event() {
                    "Synchronous debug fault.".to_string()
                } else if hfsr.escalation_forced() {
                    let description = "Escalated ";
                    let cfsr = Cfsr(memory.read_word_32(Cfsr::get_mmio_address())?);
                    if let Some(source) = cfsr.usage_fault_description()? {
                        format!("{description}{source}")
                    } else if let Some(source) = cfsr.bus_fault_description(memory)? {
                        format!("{description}{source}")
                    } else if let Some(source) = cfsr.memory_management_fault_description(memory)? {
                        format!("{description}{source}")
                    } else {
                        format!("{description}from an unknown source")
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
            ExceptionReason::SecureFault => {
                if let Some(source) = Sfsr(memory.read_word_32(Sfsr::get_mmio_address())?)
                    .secure_fault_description(memory)?
                {
                    Ok(source)
                } else {
                    Ok("SecureFault <Cause: Unknown>".to_string())
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
}
pub struct ArmV8MExceptionHandler;

impl ExceptionInterface for ArmV8MExceptionHandler {
    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        _raw_exception: u32,
    ) -> Result<DebugRegisters, DebugError> {
        let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];
        let stack_frame_return_address: u32 = get_stack_frame_return_address(stackframe_registers)?;
        let exc_return = ExcReturn(stack_frame_return_address);

        let sp_value = if exc_return.is_exception_flag() == 0xFF {
            let stack_info = (
                exc_return.use_secure_stack(),
                exc_return.stack_pointer_selection(),
            );

            let sp_reg_id = match stack_info {
                (false, false) => 0b00011000, // non-secure, main stack pointer
                (false, true) => 0b00011001,  // non-secure, process stack pointer
                (true, false) => 0b00011010,  // secure, main stack pointer
                (true, true) => 0b00011011,   // secure, process stack pointer
            };
            stackframe_registers
                .get_register(sp_reg_id.into())
                .ok_or_else(|| {
                    Error::Register(
                        "No Stack Pointer register. Please report this as a bug.".to_string(),
                    )
                })?
                .value
                .ok_or_else(|| {
                    Error::Register(
                        "No value for Stack Pointer register. Please report this as a bug."
                            .to_string(),
                    )
                })?
                .try_into()?
        } else {
            stackframe_registers.get_register_value_by_role(&RegisterRole::StackPointer)?
        };

        memory_interface.read_32(sp_value, &mut calling_stack_registers)?;
        let mut calling_frame_registers = stackframe_registers.clone();
        for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
            calling_frame_registers
                .get_register_mut_by_role(register_role)?
                .value = Some(RegisterValue::U32(calling_stack_registers[i]));
        }
        Ok(calling_frame_registers)
    }

    fn raw_exception(&self, stackframe_registers: &DebugRegisters) -> Result<u32, DebugError> {
        // Load the provided xPSR register as a bitfield.
        let exception_number = Xpsr(
            stackframe_registers.get_register_value_by_role(&RegisterRole::ProcessorStatus)? as u32,
        )
        .exception_number();

        Ok(exception_number)
    }

    fn exception_description(
        &self,
        raw_exception: u32,
        memory_interface: &mut dyn MemoryInterface,
    ) -> Result<String, DebugError> {
        ExceptionReason::from(raw_exception).expanded_description(memory_interface)
    }

    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        let stack_frame_return_address: u32 = get_stack_frame_return_address(stackframe_registers)?;
        if ExcReturn(stack_frame_return_address).is_exception_flag() == 0xFF {
            // This is an exception frame.

            let raw_exception = self.raw_exception(stackframe_registers)?;
            let description = self.exception_description(raw_exception, memory_interface)?;
            let registers = self.calling_frame_registers(
                memory_interface,
                stackframe_registers,
                raw_exception,
            )?;

            let exception_frame_pc =
                registers.get_register_value_by_role(&RegisterRole::ProgramCounter)?;

            let handler_frame = StackFrame {
                id: get_object_reference(),
                function_name: description.clone(),
                source_location: None,
                registers,
                pc: RegisterValue::U32(exception_frame_pc as u32),
                frame_base: None,
                is_inlined: false,
                local_variables: None,
                canonical_frame_address: None,
            };

            Ok(Some(ExceptionInfo {
                raw_exception,
                description,
                handler_frame,
            }))
        } else {
            // This is a normal function return.
            Ok(None)
        }
    }
}

fn get_stack_frame_return_address(stackframe_registers: &DebugRegisters) -> Result<u32, Error> {
    let return_address: u32 = stackframe_registers
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

    Ok(return_address)
}
