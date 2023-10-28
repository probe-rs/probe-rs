//! This module contains the implementation of the [`crate::core::ExceptionInterface`] for the various ARM core variants.

use crate::{
    core::{ExceptionInfo, ExceptionInterface},
    debug::DebugRegisters,
    Error, MemoryInterface,
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
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn MemoryInterface,
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
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        armv6m_armv7m_shared::exception_details(self, memory_interface, stackframe_registers)
    }

    fn calling_frame_registers(
        &self,
        memory_interface: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        armv6m_armv7m_shared::calling_frame_registers(memory_interface, stackframe_registers)
    }

    fn exception_description(
        &self,
        memory_interface: &mut dyn MemoryInterface,
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
    use std::collections::BTreeMap;

    use super::ArmV6MExceptionHandler;
    use crate::{
        architecture::arm::core::registers::cortex_m::{CORTEX_M_CORE_REGISTERS, PC, RA, SP, XPSR},
        core::ExceptionInterface,
        debug::{DebugRegister, DebugRegisters},
        MemoryInterface, RegisterValue,
    };

    #[derive(Debug, PartialEq)]
    enum MockMemoryEntry {
        Start(usize),
        End(usize),
    }

    #[derive(Debug)]
    struct MockMemory {
        ranges: BTreeMap<u64, MockMemoryEntry>,

        values: Vec<Vec<u8>>,
    }

    impl MockMemory {
        fn new() -> Self {
            MockMemory {
                ranges: BTreeMap::new(),
                values: Vec::new(),
            }
        }

        fn add_range(&mut self, address: u64, data: Vec<u8>) {
            assert!(!data.is_empty());

            let range = address..(address + data.len() as u64);

            let existing_entries = self.ranges.range(range.clone());

            let entries: Vec<_> = existing_entries.into_iter().collect();

            if entries.is_empty() {
                let new_index = self.values.len();
                self.values.push(data);

                self.ranges
                    .insert(range.start, MockMemoryEntry::Start(new_index));
                self.ranges
                    .insert(range.end - 1, MockMemoryEntry::End(new_index));
            } else {
                panic!("New range would overlap {} entries", entries.len());
            }
        }

        fn add_word_range(&mut self, address: u64, data: &[u32]) {
            let mut bytes = Vec::with_capacity(data.len() * 4);

            for word in data {
                bytes.extend_from_slice(&word.to_le_bytes());
            }

            self.add_range(address, bytes);
        }
    }

    impl MemoryInterface for MockMemory {
        fn supports_native_64bit_access(&mut self) -> bool {
            todo!()
        }

        fn read_word_64(&mut self, _address: u64) -> anyhow::Result<u64, crate::Error> {
            todo!()
        }

        fn read_word_32(&mut self, _address: u64) -> anyhow::Result<u32, crate::Error> {
            todo!()
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
            let range = address..address + data.len() as u64;

            assert!(!data.is_empty());

            assert!(
                range.start <= *self.ranges.last_key_value().unwrap().0,
                "No entries for range {:#010x} - {:#010x}",
                address,
                address + data.len() as u64
            );

            assert!(
                range.end >= *self.ranges.first_key_value().unwrap().0,
                "No entries for range {:#010x} - {:#010x} (first key: {:#010x}, end: {:#010x})",
                address,
                address + data.len() as u64,
                self.ranges.first_key_value().unwrap().0,
                range.end,
            );

            let mut entries = self.ranges.range(range);

            let (entry_addr, entry) = entries.next().unwrap_or_else(|| {
                panic!(
                    "No entries for range {:#010x} - {:#010x}",
                    address,
                    address + data.len() as u64
                )
            });

            match entry {
                MockMemoryEntry::Start(index) if *entry_addr == address => {
                    let stored_data = &self.values[*index];

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
                MockMemoryEntry::Start(_) => {
                    panic!(
                        "Missing data for range {:010x} - {:010x}",
                        address, entry_addr
                    );
                }
                MockMemoryEntry::End(index) => {
                    // In this case, we know that the corresponding start entry is before address,
                    // otherwise we would have found that one.

                    let stored_data = &self.values[*index];

                    let end_addr = entry_addr;
                    let _entry_addr = end_addr - (stored_data.len() as u64 - 1);

                    todo!()
                }
            }
        }

        fn write_word_64(&mut self, _address: u64, _data: u64) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn write_word_32(&mut self, _address: u64, _data: u32) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn write_word_8(&mut self, _address: u64, _data: u8) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn write_64(&mut self, _address: u64, _data: &[u64]) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn write_32(&mut self, _address: u64, _data: &[u32]) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn write_8(&mut self, _address: u64, _data: &[u8]) -> anyhow::Result<(), crate::Error> {
            todo!()
        }

        fn supports_8bit_transfers(&self) -> anyhow::Result<bool, crate::Error> {
            todo!()
        }

        fn flush(&mut self) -> anyhow::Result<(), crate::Error> {
            todo!()
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
                value: Some(RegisterValue::U32(inital_sp)), // TODO: Why is this not changed here, the stack pointer does actually change
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
