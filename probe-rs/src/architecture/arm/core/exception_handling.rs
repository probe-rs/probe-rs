//! This module contains the implementation of the [`crate::core::ExceptionInterface`] for the various ARM core variants.

use crate::{
    core::{ExceptionInfo, ExceptionInterface},
    debug::DebugRegisters,
    memory::ReadOnlyMemoryInterface,
    Error,
};
pub(crate) mod armv6m;
/// Where applicable, this defines shared logic for implementing exception handling accross the various ARMv6-m and ARMv7-m [`crate::CoreType`]'s.
pub(crate) mod armv6m_armv7m_shared;
// NOTE: There is also a [`CoreType::Armv7em`] variant, but it is not currently used/implemented in probe-rs.
pub(crate) mod armv7m;

pub(crate) mod armv8m;

/// Exception handling for cores based on the ARMv6-M architecture.
pub struct ArmV6MExceptionHandler {}

impl ExceptionInterface for ArmV6MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        crate::architecture::arm::core::exception_handling::armv6m::exception_description(
            memory_interface,
            stackframe_registers,
        )
    }
}

/// Exception handling for cores based on the ARMv7-M and ARMv7-EM architectures.
pub struct ArmV7MExceptionHandler {}

impl ExceptionInterface for ArmV7MExceptionHandler {
    fn exception_details(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn ReadOnlyMemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        // Load the provided xPSR register as a bitfield.
        let exception_number = armv6m_armv7m_shared::Xpsr(
            stackframe_registers
                .get_register_value_by_role(&crate::core::RegisterRole::ProcessorStatus)?
                as u32,
        )
        .exception_number();

        Ok(format!(
            "{:?}",
            armv7m::ExceptionReason::from(exception_number)
                .expanded_description(memory_interface)?
        ))
    }
}

#[cfg(test)]
mod test {
    use pretty_assertions::assert_eq;
    use std::path::{Path, PathBuf};

    use super::ArmV6MExceptionHandler;
    use crate::{
        architecture::arm::core::registers::cortex_m::{CORTEX_M_CORE_REGISTERS, PC, RA, SP, XPSR},
        core::ExceptionInterface,
        debug::{DebugInfo, DebugRegister, DebugRegisters},
        memory::ReadOnlyMemoryInterface,
        RegisterValue,
    };

    #[derive(Debug)]
    struct MockMemory {
        /// Sorted list of ranges
        values: Vec<(u64, Vec<u8>)>,
    }

    impl MockMemory {
        fn new() -> Self {
            MockMemory { values: Vec::new() }
        }

        fn add_range(&mut self, address: u64, data: Vec<u8>) {
            assert!(!data.is_empty());

            match self
                .values
                .binary_search_by_key(&address, |(addr, _data)| *addr)
            {
                Ok(index) => {
                    panic!("Failed to add data at {:#010x} - {:#010x}, already exists at {:#010x} - {:#010x}", address, address + data.len() as u64, self.values[index].0, self.values[index].0 + self.values[index].1.len() as u64);
                }
                Err(index) => {
                    // This is the index where the new entry should be inserted,
                    // but we first have to check on both sides, if this would overlap with existing entries

                    if index > 0 {
                        let previous_entry = &self.values[index - 1];

                        assert!(
                            previous_entry.0 + previous_entry.1.len() as u64 <= address,
                            "Failed to add data at {:#010x} - {:#010x}, overlaps with existing entry at {:#010x} - {:#010x}",
                            address,
                            address + data.len() as u64,
                            previous_entry.0,
                            previous_entry.0 + previous_entry.1.len() as u64
                        );
                    }

                    if index + 1 < self.values.len() {
                        let next_entry = &self.values[index + 1];

                        assert!(
                            next_entry.0 >= address + data.len() as u64,
                            "Failed to add data at {:#010x} - {:#010x}, overlaps with existing entry at {:#010x} - {:#010x}",
                            address,
                            address + data.len() as u64,
                            next_entry.0,
                            next_entry.0 + next_entry.1.len() as u64
                        );
                    }

                    self.values.insert(index, (address, data));
                }
            }
        }

        fn add_word_range(&mut self, address: u64, data: &[u32]) {
            let mut bytes = Vec::with_capacity(data.len() * 4);

            for word in data {
                bytes.extend_from_slice(&word.to_le_bytes());
            }

            self.add_range(address, bytes);
        }

        fn missing_range(&self, start: u64, end: u64) -> ! {
            panic!("No entry for range {:#010x} - {:#010x}", start, end);
        }
    }

    impl ReadOnlyMemoryInterface for MockMemory {
        fn supports_native_64bit_access(&mut self) -> bool {
            false
        }

        fn read_word_64(&mut self, _address: u64) -> anyhow::Result<u64, crate::Error> {
            todo!()
        }

        fn read_word_32(&mut self, address: u64) -> anyhow::Result<u32, crate::Error> {
            let mut bytes = [0u8; 4];
            self.read_8(address, &mut bytes)?;

            Ok(u32::from_le_bytes(bytes))
        }

        fn read_word_8(&mut self, _address: u64) -> anyhow::Result<u8, crate::Error> {
            todo!()
        }

        fn read_64(
            &mut self,
            _address: u64,
            _data: &mut [u64],
        ) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn read_32(&mut self, address: u64, data: &mut [u32]) -> anyhow::Result<(), crate::Error> {
            let mut buff = vec![0u8; data.len() * 4];

            self.read_8(address, &mut buff)?;

            for (i, chunk) in buff.chunks_exact(4).enumerate() {
                data[i] = u32::from_le_bytes(chunk.try_into().unwrap());
            }

            Ok(())
        }

        fn read_8(&mut self, address: u64, data: &mut [u8]) -> anyhow::Result<(), crate::Error> {
            let stored_data = match self
                .values
                .binary_search_by_key(&address, |(addr, _data)| *addr)
            {
                Ok(index) => {
                    // Found entry with matching start address

                    &self.values[index].1
                }
                Err(0) => self.missing_range(address, address + data.len() as u64),
                Err(index) => {
                    let previous_entry = &self.values[index - 1];

                    // address:        10  - 12
                    // previous_entry  8   - 11

                    // reading from 10 -> reading from 8 + 2

                    let offset = address - previous_entry.0;

                    if offset >= previous_entry.1.len() as u64 {
                        // The requested range is not covered by the previous entry
                        self.missing_range(address, address + data.len() as u64)
                    }

                    &previous_entry.1[offset as usize..]
                }
            };

            if stored_data.len() >= data.len() {
                data.copy_from_slice(&stored_data[..data.len()]);
                Ok(())
            } else {
                data[..stored_data.len()].copy_from_slice(stored_data);

                self.read_8(
                    address + stored_data.len() as u64,
                    &mut data[stored_data.len()..],
                )
            }
        }

        fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
            Ok(true)
        }
    }

    #[test]
    fn mock_memory_read() {
        let mut mock_memory = MockMemory::new();

        let values = [
            0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000, 0x0000017f, 0x00000180,
            0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
        ];

        mock_memory.add_word_range(0x2001_ffd0, &values);

        for (offset, expected) in values.iter().enumerate() {
            let actual = mock_memory
                .read_word_32(0x2001_ffd0 + (offset * 4) as u64)
                .unwrap();

            assert_eq!(actual, *expected);
        }
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

        let description = handler
            .exception_description(&mut memory, &registers)
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

    #[test]
    fn unwinding_first_instruction_after_exception() {
        let path = Path::new("./exceptions");

        let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let path = base_dir.join(path);

        println!("Path: {}", path.display());

        let debug_info = DebugInfo::from_file(&path).unwrap();

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001fff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffd0
        // R14       : 0xfffffff9
        // R15       : 0x00000182
        // MSP       : 0x2001ffd0
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000
        // FPSCR     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ffcf, // R1
            0x20000044, // R2
            0x20000044, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001fff0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffd0, // R13
            0xfffffff9, // R14
            0x00000182, // R15
            0x2001ffd0, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let expected_regs = regs.clone();

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2001_ffd0,
            &[
                0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000, 0x0000017f, 0x00000180,
                0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind_impl(
                regs,
                &mut dummy_mem,
                exception_handler,
                Some(probe_rs_target::InstructionSet::Thumb2),
            )
            .unwrap();

        let first_frame = &frames[0];

        assert_eq!(first_frame.pc, RegisterValue::U32(0x00000182));

        assert_eq!(
            first_frame.function_name,
            "__cortex_m_rt_SVCall_trampoline".to_string()
        );

        assert_eq!(first_frame.registers, expected_regs);

        let next_frame = &frames[1];
        assert_eq!(next_frame.function_name, "SVCall");
        assert_eq!(next_frame.pc, RegisterValue::U32(0x00000182));

        // Expected stack frame(s):
        // Frame 0: __cortex_m_rt_SVCall_trampoline @ 0x00000182
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:22:1
        //
        // <--- A frame seems to be missing here, to indicate the exception entry
        //
        // Frame 1: __cortex_m_rt_main @ 0x00000180   (<--- This should be 0x17e)
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:19:5
        // Frame 2: __cortex_m_rt_main_trampoline @ 0x00000160
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:11:1
        // Frame 3: memmove @ 0x0000013c
        // Frame 4: memmove @ 0x0000013c

        // Registers in frame 1:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001fff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001fff0
        // R14       : 0x0000017f
        // R15       : 0x0000017e
        // MSP       : 0x2001fff0
        // PSP       : 0x00000000
        // XPSR      : 0x21000000
        // EXTRA     : 0x00000000
        // XPSR      : 0x21000000
    }

    #[test]
    fn unwinding_in_exception_handler() {
        let path = Path::new("./exceptions");

        let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let path = base_dir.join(path);

        println!("Path: {}", path.display());

        let debug_info = DebugInfo::from_file(&path).unwrap();

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ff9f
        // R2        : 0x20000047
        // R3        : 0x20000047
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001ffc0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffc0
        // R14       : 0x0000042f
        // R15       : 0x000001a4
        // MSP       : 0x2001ffc0
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ff9f, // R1
            0x20000047, // R2
            0x20000047, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001ffc0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffc0, // R13
            0x0000042f, // R14
            0x000001a4, // R15
            0x2001ffc0, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x2001ffc0 = 0x2001ffc8
        // 0x2001ffc4 = 0x0000018b
        // 0x2001ffc8 = 0x2001fff0
        // 0x2001ffcc = 0xfffffff9
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2001_ffc0,
            &[
                0x2001ffc8, 0x0000018b, 0x2001fff0, 0xfffffff9, 0x00000001, 0x2001ffcf, 0x20000044,
                0x20000044, 0x00000000, 0x0000017f, 0x00000180, 0x21000000, 0x2001fff8, 0x00000161,
                0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind_impl(
                regs,
                &mut dummy_mem,
                exception_handler,
                Some(probe_rs_target::InstructionSet::Thumb2),
            )
            .unwrap();

        assert_eq!(frames[0].pc, RegisterValue::U32(0x000001a4));

        assert_eq!(
            frames[1].function_name,
            "__cortex_m_rt_SVCall_trampoline".to_string()
        );

        assert_eq!(frames[1].pc, RegisterValue::U32(0x0000018A)); // <-- This seems wrong, this is the instruction *after* the jump into the topmost frame

        assert_eq!(
            frames[1]
                .registers
                .get_frame_pointer()
                .and_then(|r| r.value),
            Some(RegisterValue::U32(0x2001ffc8))
        );

        let printed_backtrace = frames
            .into_iter()
            .map(|f| f.to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }

    #[test]
    fn unwinding_in_exception_trampoline() {
        let path = Path::new("./exceptions");

        let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let path = base_dir.join(path);

        println!("Path: {}", path.display());

        let debug_info = DebugInfo::from_file(&path).unwrap();

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001fff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffc8
        // R14       : 0xfffffff9
        // R15       : 0x00000184
        // MSP       : 0x2001ffc8
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000
        // FPSCR     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ffcf, // R1
            0x20000044, // R2
            0x20000044, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001fff0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffc8, // R13
            0xfffffff9, // R14
            0x00000184, // R15
            0x2001ffc8, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x2001ffc8 = 0x2001fff0
        // 0x2001ffcc = 0xfffffff9
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2001_ffc8,
            &[
                0x2001fff0, 0xfffffff9, 0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000,
                0x0000017f, 0x00000180, 0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind_impl(
                regs,
                &mut dummy_mem,
                exception_handler,
                Some(probe_rs_target::InstructionSet::Thumb2),
            )
            .unwrap();

        let printed_backtrace = frames
            .into_iter()
            .map(|f| f.to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }
}
