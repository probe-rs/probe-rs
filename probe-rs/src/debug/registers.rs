use crate::core::Core;

use probe_rs_target::Architecture;

use std::collections::HashMap;

use crate::core::{RegisterDescription, RegisterFile};

/// All the register information currently available.
#[derive(Debug, Clone, PartialEq)]
pub struct Registers {
    pub(crate) register_description: &'static RegisterFile,

    pub(crate) values: HashMap<u32, u64>,

    pub(crate) architecture: Architecture,

    pub(crate) address_size: usize,
}

impl Registers {
    /// Read all registers from the given core.
    pub fn from_core(core: &mut Core) -> Self {
        let register_file = core.registers();

        let num_platform_registers = register_file.platform_registers.len();
        let pc_desc = register_file.program_counter();

        let mut registers = Registers {
            register_description: register_file,
            values: HashMap::new(),
            architecture: core.architecture(),
            address_size: pc_desc.size_in_bytes(),
        };

        for i in 0..num_platform_registers {
            let result: Result<u64, crate::Error> =
                core.read_core_reg(register_file.platform_register(i));
            match result {
                Ok(value) => registers.values.insert(i as u32, value),
                Err(e) => {
                    log::warn!("Failed to read value for register {}: {}", i, e);
                    None
                }
            };
        }
        registers
    }

    /// Gets the address size for this target, in bytes
    pub fn get_address_size_bytes(&self) -> usize {
        self.address_size
    }

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn get_frame_pointer(&self) -> Option<u64> {
        let reg_num = self.register_description.frame_pointer().location.0 as u32;

        self.values.get(&reg_num).copied()
    }
    /// Set the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn set_frame_pointer(&mut self, value: Option<u64>) {
        let register_address = self.register_description.frame_pointer().location.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the program counter.
    pub fn get_program_counter(&self) -> Option<u64> {
        let reg_num = self.register_description.program_counter().location.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the program counter.
    pub fn set_program_counter(&mut self, value: Option<u64>) {
        let register_address = self.register_description.program_counter().location.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the stack pointer.
    pub fn get_stack_pointer(&self) -> Option<u64> {
        let reg_num = self.register_description.stack_pointer().location.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the stack pointer.
    pub fn set_stack_pointer(&mut self, value: Option<u64>) {
        let register_address = self.register_description.stack_pointer().location.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the return address.
    pub fn get_return_address(&self) -> Option<u64> {
        let reg_num = self.register_description.return_address().location.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the return address.
    pub fn set_return_address(&mut self, value: Option<u64>) {
        let register_address = self.register_description.return_address().location.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the value using the dwarf register number as an index.
    pub fn get_value_by_dwarf_register_number(&self, register_number: u32) -> Option<u64> {
        self.values.get(&register_number).copied()
    }

    /// Lookup the register name from the RegisterDescriptions.
    pub fn get_name_by_dwarf_register_number(&self, register_number: u32) -> Option<String> {
        self.register_description
            .get_platform_register(register_number as usize)
            .map(|platform_register| platform_register.name().to_string())
    }

    /// Set the value using the dwarf register number as an index.
    pub fn set_by_dwarf_register_number(&mut self, register_number: u32, value: Option<u64>) {
        if let Some(value) = value {
            self.values.insert(register_number, value);
        } else {
            self.values.remove(&register_number);
        }
    }

    /// Lookup the RegisterDescription for a register
    pub fn get_description_by_dwarf_register_number(
        &self,
        register_number: u32,
    ) -> Option<&RegisterDescription> {
        self.register_description
            .get_platform_register(register_number as usize)
    }

    /// Returns an iterator over all register numbers and their values.
    pub fn registers(&self) -> impl Iterator<Item = (&u32, &u64)> {
        self.values.iter()
    }
}
