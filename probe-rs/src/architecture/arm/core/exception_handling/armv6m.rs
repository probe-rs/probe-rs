use crate::{
    core::{ExceptionInfo, ExceptionInterface},
    debug::DebugRegisters,
    Error, MemoryInterface,
};

use super::armv6m_armv7m_shared::{self};

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

/// Exception handling for cores based on the ARMv6-M architecture.
pub struct ArmV6MExceptionHandler {}

impl ExceptionInterface for ArmV6MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn raw_exception(
        &self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, Error> {
        armv6m_armv7m_shared::raw_exception(stackframe_registers)
    }

    fn exception_description(
        &self,
        raw_exception: u32,
        _memory_interface: &mut dyn MemoryInterface,
    ) -> Result<String, Error> {
        // TODO: Some ARMv6-M cores (e.g. the Cortex-M0) do not have HFSR and CFGR registers, so we cannot
        //       determine the cause of the hard fault. We should add a check for this, and return a more
        //       helpful error message in this case (I'm not sure this is possible).
        //       Until then, this will return a generic error message for all hard faults on this architecture.
        Ok(format!("{:?}", ExceptionReason::from(raw_exception)))
    }
}

#[cfg(test)]
mod test {
    use pretty_assertions::assert_eq;

    use super::ArmV6MExceptionHandler;
    use crate::{
        architecture::arm::core::registers::cortex_m::{CORTEX_M_CORE_REGISTERS, PC, RA, SP, XPSR},
        core::ExceptionInterface,
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

        assert_eq!(description, "ThreadMode")
    }

    #[test]
    fn exception_handler_no_exception_details() {
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
            value: Some(RegisterValue::U32(0x1000_0000)),
        });

        let details = handler.exception_details(&mut memory, &registers).unwrap();

        assert_eq!(details, None)
    }

    #[test]
    fn exception_handler_hardfault_details() {
        let handler = ArmV6MExceptionHandler {};

        let mut memory = MockMemory::new();

        let inital_sp: u32 = 0x2000_1000;

        let stack_return_address = 0x20_00;
        let stack_program_counter = 0x1000_0000;
        let stack_xpsr = 15;

        memory.add_word_range(
            inital_sp as u64,
            &[
                0x11_00,               // R0
                0x11_01,               // R1
                0x11_02,               // R2,
                0x11_03,               // R3,
                0x11_12,               // R12,
                stack_return_address,  // LR,
                stack_program_counter, //return address  (next address after return from exception)
                stack_xpsr,            // XPSR (interrupt = 15)
            ],
        );

        println!("{:#x?}", memory);

        let mut registers = DebugRegisters(vec![]);

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &XPSR,
            value: Some(RegisterValue::U32(3)),
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &RA,
            value: Some(RegisterValue::U32(0xffff_fff9)),
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &SP,
            value: Some(RegisterValue::U32(inital_sp)),
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: CORTEX_M_CORE_REGISTERS.core_register(0),
            value: None,
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: CORTEX_M_CORE_REGISTERS.core_register(1),
            value: None,
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: CORTEX_M_CORE_REGISTERS.core_register(2),
            value: None,
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: CORTEX_M_CORE_REGISTERS.core_register(3),
            value: None,
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: CORTEX_M_CORE_REGISTERS.core_register(12),
            value: None,
        });

        registers.0.push(DebugRegister {
            dwarf_id: None,
            core_register: &PC,
            value: None,
        });

        let details = handler
            .exception_details(&mut memory, &registers)
            .expect("Should be able to get exception info");

        let details = details.expect("Should detect an exception");

        assert_eq!(details.description, "HardFault");

        let mut expected_registers = DebugRegisters(vec![
            DebugRegister {
                dwarf_id: None,
                core_register: CORTEX_M_CORE_REGISTERS.core_register(0),
                value: Some(RegisterValue::U32(0x11_00)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: CORTEX_M_CORE_REGISTERS.core_register(1),
                value: Some(RegisterValue::U32(0x11_01)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: CORTEX_M_CORE_REGISTERS.core_register(2),
                value: Some(RegisterValue::U32(0x11_02)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: CORTEX_M_CORE_REGISTERS.core_register(3),
                value: Some(RegisterValue::U32(0x11_03)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: CORTEX_M_CORE_REGISTERS.core_register(12),
                value: Some(RegisterValue::U32(0x11_12)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: &SP,
                value: Some(RegisterValue::U32(inital_sp + 0x20)), // Stack pointer has to be adjusted to account for the registers stored on the stack
            },
            DebugRegister {
                dwarf_id: None,
                core_register: &RA,
                value: Some(RegisterValue::U32(stack_return_address)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: &PC,
                value: Some(RegisterValue::U32(stack_program_counter)),
            },
            DebugRegister {
                dwarf_id: None,
                core_register: &XPSR,
                value: Some(RegisterValue::U32(stack_xpsr)),
            },
        ]);

        let mut actual_registers = details.calling_frame_registers;
        actual_registers.0.sort_by_key(|r| r.core_register);
        expected_registers.0.sort_by_key(|r| r.core_register);

        for (actual, expected) in actual_registers.0.iter().zip(expected_registers.0.iter()) {
            assert_eq!(actual, expected);
        }

        assert_eq!(actual_registers.0.len(), expected_registers.0.len());
    }
}
