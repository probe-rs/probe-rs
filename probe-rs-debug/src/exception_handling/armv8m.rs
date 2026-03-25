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

// TODO don't copy paste this
pub enum SecurityExtension {
    NotImplemented,
    Implemented,
    ImplementedWithStateHandling,
    Reserved,
}

impl From<u8> for SecurityExtension {
    fn from(value: u8) -> Self {
        match value {
            0b0000 => SecurityExtension::NotImplemented,
            0b0001 => SecurityExtension::Implemented,
            0b0011 => SecurityExtension::ImplementedWithStateHandling,
            _ => SecurityExtension::Reserved,
        }
    }
}

memory_mapped_bitfield_register! {
    /// Processor Feature Register 1
    pub struct IdPfr1(u32);
    0xE000_ED44, "ID_PFR1",
    impl From;
    /// Identifies support for the M-Profile programmer's model
    pub u8, m_prog_mod, _: 11, 8;
    /// Identifies whether the Security Extension is implemented
    pub u8, security, _: 7, 4;
}

impl IdPfr1 {
    pub fn security_present(&self) -> bool {
        matches!(
            self.security().into(),
            SecurityExtension::Implemented | SecurityExtension::ImplementedWithStateHandling
        )
    }
}

pub struct ArmV8MExceptionHandler;

impl ExceptionInterface for ArmV8MExceptionHandler {
    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        _callee_frame_registers: &DebugRegisters,
        _raw_exception: u32,
    ) -> Result<DebugRegisters, DebugError> {
        let mut calling_stack_registers = vec![0u32; EXCEPTION_STACK_REGISTERS.len()];
        let stack_frame_return_address: u32 = get_stack_frame_return_address(stackframe_registers)?;
        let exc_return = ExcReturn(stack_frame_return_address);
        let idpfr1 = IdPfr1(memory_interface.read_word_32(IdPfr1::get_mmio_address())?);
        let secure = idpfr1.security_present();

        tracing::trace!(
            "v8m exception unwind: EXC_RETURN={:#010x} S={} DCRS={} FType={} Mode={} SPSEL={} ES={} security_ext={}",
            stack_frame_return_address,
            exc_return.use_secure_stack(),
            exc_return.use_default_register_stacking(),
            exc_return.use_standard_stackframe(),
            exc_return.mode(),
            exc_return.stack_pointer_selection(),
            exc_return.exception_secure(),
            secure,
        );

        let sp_value: u64 = if exc_return.is_exception_flag() == 0xFF {
            // EXC_RETURN, returning from an exception.
            //
            // For the SP to read the exception frame from, we need to distinguish:
            // - SPSEL=1 (PSP): the exception frame is on PSP, which is a different
            //   stack from the handler's MSP. The hardware PSP register is correct
            //   because the handler doesn't modify PSP.
            // - SPSEL=0 (MSP): the exception frame is on MSP, the same stack the
            //   handler uses. The hardware MSP includes the handler's own pushes.
            //   We must use the DWARF-unwound generic SP, which gives us the
            //   handler's entry SP = the top of the exception frame.
            if exc_return.stack_pointer_selection() {
                // SPSEL=1: exception frame is on PSP (different stack from handler).
                // Read from the hardware PSP register.
                let sp_reg_id = if secure {
                    if exc_return.use_secure_stack() {
                        0b00011011u16 // PSP_S
                    } else {
                        0b00011001 // PSP_NS
                    }
                } else {
                    0b00010010 // PSP
                };

                let reg = stackframe_registers
                    .get_register(sp_reg_id.into())
                    .ok_or_else(|| {
                        Error::Register(format!(
                            "No Stack Pointer register with id {sp_reg_id:#04x}. Please report this as a bug."
                        ))
                    })?;
                tracing::trace!(
                    "v8m exception unwind: SPSEL=1, using hardware PSP register '{}' (id={:#04x}) = {:?}",
                    reg.core_register.name(),
                    sp_reg_id,
                    reg.value,
                );
                let val: u64 = reg
                    .value
                    .ok_or_else(|| {
                        Error::Register(format!(
                            "No value for Stack Pointer register '{}'. Please report this as a bug.",
                            reg.core_register.name()
                        ))
                    })?
                    .try_into()?;
                val
            } else {
                // SPSEL=0: exception frame is on MSP (same stack as handler).
                // Use the DWARF-unwound generic SP from stackframe_registers,
                // which is the handler's entry SP = top of the exception frame.
                let sp =
                    stackframe_registers.get_register_value_by_role(&RegisterRole::StackPointer)?;
                tracing::trace!(
                    "v8m exception unwind: SPSEL=0, using DWARF-unwound generic SP = {:#010x}",
                    sp,
                );
                sp
            }
        } else if exc_return.is_exception_flag() == 0xFE {
            // FNC_RETURN, returning from a secure -> non-secure function call
            // get SPSEL from CONTROL_S, unstack ReturnAddress (and retpsr?)
            todo!()
        } else {
            stackframe_registers.get_register_value_by_role(&RegisterRole::StackPointer)?
        };

        // Determine offset to state context based on whether additional state context
        // (integrity signature + R4-R11) was stacked.
        // DCRS=1 means default stacking rules (no additional state context for non-secure).
        // DCRS=0 means additional state context was stacked (0x28 bytes before state context).
        let additional_state_context_size: u64 = if !exc_return.use_default_register_stacking() {
            // DCRS=0: additional state context (integrity sig + reserved + R4-R11) = 0x28 bytes
            0x28
        } else {
            0
        };

        let state_context_addr = sp_value + additional_state_context_size;
        tracing::trace!(
            "v8m exception unwind: sp_value={:#010x} additional_state_context_size={:#x} state_context_addr={:#010x}",
            sp_value,
            additional_state_context_size,
            state_context_addr,
        );

        memory_interface.read_32(state_context_addr, &mut calling_stack_registers)?;
        tracing::trace!(
            "v8m exception unwind: stacked registers: R0={:#010x} R1={:#010x} R2={:#010x} R3={:#010x} R12={:#010x} LR={:#010x} PC={:#010x} xPSR={:#010x}",
            calling_stack_registers[0],
            calling_stack_registers[1],
            calling_stack_registers[2],
            calling_stack_registers[3],
            calling_stack_registers[4],
            calling_stack_registers[5],
            calling_stack_registers[6],
            calling_stack_registers[7],
        );

        let mut calling_frame_registers = stackframe_registers.clone();

        // Overwrite the stacked registers (R0-R3, R12, LR, PC, xPSR) with values
        // read from the exception frame on the stack.
        for (i, register_role) in EXCEPTION_STACK_REGISTERS.iter().enumerate() {
            calling_frame_registers
                .get_register_mut_by_role(register_role)?
                .value = Some(RegisterValue::U32(calling_stack_registers[i]));
        }

        // Set the generic SP to the source stack pointer value.
        // The handler's generic SP (from DWARF unwinding) is MSP, but the exception
        // frame may be on PSP. We set SP to the source stack value here;
        // exception_details will later advance it past the exception frame.
        let generic_sp =
            calling_frame_registers.get_register_mut_by_role(&RegisterRole::StackPointer)?;
        generic_sp.value = Some(RegisterValue::U32(sp_value as u32));
        tracing::trace!(
            "v8m calling_frame_registers: set generic SP to source stack value {:#010x}",
            sp_value,
        );

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
        callee_frame_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, DebugError> {
        let stack_frame_return_address: u32 = get_stack_frame_return_address(stackframe_registers)?;
        let exc_return = ExcReturn(stack_frame_return_address);

        tracing::trace!(
            "v8m exception_details: LR={:#010x} is_exception_flag={:#04x}",
            stack_frame_return_address,
            exc_return.is_exception_flag(),
        );

        if exc_return.is_exception_flag() != 0xFF {
            // This is a normal function return.
            return Ok(None);
        }

        // This is an exception frame.
        let raw_exception = self.raw_exception(stackframe_registers)?;
        let description = self.exception_description(raw_exception, memory_interface)?;
        tracing::trace!(
            "v8m exception_details: raw_exception={} description={}",
            raw_exception,
            description,
        );

        let mut registers = self.calling_frame_registers(
            memory_interface,
            stackframe_registers,
            callee_frame_registers,
            raw_exception,
        )?;

        let exception_frame_pc =
            registers.get_register_value_by_role(&RegisterRole::ProgramCounter)?;

        // Compute the frame size to update SP past the exception frame.
        // See ARMv8-M ARM section B3.19 and pseudocode PushStack/PushCalleeStack.
        //
        // The exception frame layout from SP upward is:
        //   [Additional state context: integrity sig + reserved + R4-R11 = 0x28 bytes] (if DCRS=0)
        //   [State context: R0,R1,R2,R3,R12,LR,ReturnAddress,RETPSR = 0x20 bytes] (always)
        //   [FP caller context: S0-S15,FPSCR,VPR = 0x48 bytes] (if FType=0)
        //   [FP callee context: S16-S31 = 0x40 bytes] (if FType=0 and secure TS)
        //
        // DCRS (bit 5): 0 = additional state context stacked, 1 = default rules (not stacked for NS)
        // FType (bit 4): 0 = extended FP frame, 1 = standard integer-only frame

        let additional_state_size: usize = if !exc_return.use_default_register_stacking() {
            0x28
        } else {
            0
        };

        let state_context_size: usize = 0x20;

        let fp_context_size: usize = if !exc_return.use_standard_stackframe() {
            // FType=0: extended frame with FP registers (S0-S15, FPSCR, VPR = 18 words = 0x48)
            0x48
        } else {
            0
        };

        let frame_size = additional_state_size + state_context_size + fp_context_size;

        // Check RETPSR.SPREALIGN (bit 9) to determine if the stack was realigned.
        let stacked_xpsr = calling_stack_registers_xpsr(&registers)?;
        let sprealign = Xpsr(stacked_xpsr).stack_was_realigned();
        let alignment_padding: usize = if sprealign { 4 } else { 0 };

        let total_frame_size = frame_size + alignment_padding;

        tracing::trace!(
            "v8m exception_details: frame_size={:#x} sprealign={} total_frame_size={:#x}",
            frame_size,
            sprealign,
            total_frame_size,
        );

        // Update the generic SP register to point past the exception frame,
        // restoring it to the pre-exception value.
        let sp = registers.get_register_mut_by_role(&RegisterRole::StackPointer)?;
        if let Some(sp_value) = sp.value.as_mut() {
            tracing::trace!(
                "v8m exception_details: updating SP from {:#} by +{:#x}",
                sp_value,
                total_frame_size,
            );
            sp_value.increment_address(total_frame_size)?;
            tracing::trace!("v8m exception_details: new SP = {:#}", sp_value);
        }

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
    }
}

/// Extract the stacked xPSR value from the calling frame registers.
fn calling_stack_registers_xpsr(registers: &DebugRegisters) -> Result<u32, Error> {
    let xpsr = registers.get_register_value_by_role(&RegisterRole::ProcessorStatus)? as u32;
    Ok(xpsr)
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
