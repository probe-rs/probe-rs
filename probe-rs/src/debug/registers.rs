use crate::core::Core;

use probe_rs_target::Architecture;

use std::collections::HashMap;

use crate::core::RegisterFile;

/// All the register information currently available.
#[derive(Debug, Clone, PartialEq)]
pub struct Registers {
    pub(crate) register_description: &'static RegisterFile,

    // TODO 64-bit - handle larger values
    pub(crate) values: HashMap<u32, u32>,

    pub(crate) architecture: Architecture,
}

impl Registers {
    /// Read all registers from the given core.
    pub fn from_core(core: &mut Core) -> Self {
        let register_file = core.registers();

        let num_platform_registers = register_file.platform_registers.len();

        let mut registers = Registers {
            register_description: register_file,
            values: HashMap::new(),
            architecture: core.architecture(),
        };

        // TODO 64-bit - support other sizes
        for i in 0..num_platform_registers {
            let result: Result<u32, crate::Error> =
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

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn get_frame_pointer(&self) -> Option<u32> {
        let reg_num = self.register_description.frame_pointer().address.0 as u32;

        self.values.get(&reg_num).copied()
    }
    /// Set the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn set_frame_pointer(&mut self, value: Option<u32>) {
        let register_address = self.register_description.frame_pointer().address.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the program counter.
    pub fn get_program_counter(&self) -> Option<u32> {
        let reg_num = self.register_description.program_counter().address.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the program counter.
    pub fn set_program_counter(&mut self, value: Option<u32>) {
        let register_address = self.register_description.program_counter().address.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the stack pointer.
    pub fn get_stack_pointer(&self) -> Option<u32> {
        let reg_num = self.register_description.stack_pointer().address.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the stack pointer.
    pub fn set_stack_pointer(&mut self, value: Option<u32>) {
        let register_address = self.register_description.stack_pointer().address.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the return address.
    pub fn get_return_address(&self) -> Option<u32> {
        let reg_num = self.register_description.return_address().address.0 as u32;

        self.values.get(&reg_num).copied()
    }

    /// Set the return address.
    pub fn set_return_address(&mut self, value: Option<u32>) {
        let register_address = self.register_description.return_address().address.0 as u32;

        if let Some(value) = value {
            self.values.insert(register_address, value);
        } else {
            self.values.remove(&register_address);
        }
    }

    /// Get the value using the dwarf register number as an index.
    pub fn get_value_by_dwarf_register_number(&self, register_number: u32) -> Option<u32> {
        self.values.get(&register_number).copied()
    }

    /// Lookup the register name from the RegisterDescriptions.
    pub fn get_name_by_dwarf_register_number(&self, register_number: u32) -> Option<String> {
        self.register_description
            .get_platform_register(register_number as usize)
            .map(|platform_register| platform_register.name().to_string())
    }

    /// Set the value using the dwarf register number as an index.
    pub fn set_by_dwarf_register_number(&mut self, register_number: u32, value: Option<u32>) {
        if let Some(value) = value {
            self.values.insert(register_number, value);
        } else {
            self.values.remove(&register_number);
        }
    }

    /// Returns an iterator over all register numbers and their values.
    pub fn registers(&self) -> impl Iterator<Item = (&u32, &u32)> {
        self.values.iter()
    }
}
