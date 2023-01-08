use crate::{
    core::{Core, RegisterDataType, RegisterFile},
    Error, RegisterId, RegisterValue,
};

/// The group name of a register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterGroup {
    /// Core / CPU Registers. Using the term `Base` rather than `platform`, because that is what the DWARF spec calls these registers.
    Base,
    /// Argument Register
    Argument,
    /// Result Register
    Result,
    /// [`RegisterFile`] contains some register descriptions that are not part of an array, and may or may not have the same `RegisterId` as registers in other groups.
    Singleton,
}

/// Stores the relevant information from [`RegisterDescription`](crate::core::RegisterDescription)
/// as well as additional information required during debug.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugRegister {
    /// To lookup platform specific details of register definitions.
    pub register_file: &'static RegisterFile,
    /// Register definitions are grouped depending on their purpose.
    pub group: RegisterGroup,
    // TODO: Consider capturing reference to RegisterDescription, so we can delegate actions like size_in_bytes.
    /// The name of the register.
    pub name: &'static str,
    /// If a special name exists for an existing register, e.g. Arm register 'r15' is also known as 'pc' (program counter)
    pub special_name: Option<&'static str>,
    /// The location where the register is stored.
    pub id: RegisterId,
    /// [DWARF](https://dwarfstd.org) specification, section 2.6.1.1.3.1 "... operations encode the names of up to 32 registers, numbered from 0 through 31, inclusive ..."
    pub dwarf_id: Option<u16>,
    /// The type of data stored in a register.
    pub data_type: RegisterDataType,
    /// Size in bits, e.g. 32 or 64.
    pub size_in_bits: usize,
    /// The value of the register is read from the target memory and updated as needed.
    pub value: Option<RegisterValue>,
}

impl DebugRegister {
    /// Test if this is a 32-bit unsigned integer register
    pub(crate) fn is_u32(&self) -> bool {
        self.data_type == RegisterDataType::UnsignedInteger && self.size_in_bits == 32
    }

    /// A helper function to determine if the contained register value is equal to the maximum value that can be stored in that datatype.
    /// Will return false if the value is `None`
    pub(crate) fn is_max_value(&self) -> bool {
        match self.value {
            Some(register_value) => register_value.is_max_value(),
            None => false,
        }
    }

    /// A helper function to determine if the contained register value is zero.
    /// Will return false if the value is `None`
    pub(crate) fn is_zero(&self) -> bool {
        match self.value {
            Some(register_value) => register_value.is_zero(),
            None => false,
        }
    }

    /// Retrieve the special name if it exists, else the actual name using the [`RegisterId`] as an identifier.
    pub fn get_register_name(&self) -> String {
        self.special_name.unwrap_or(self.name).to_string()
    }
}

/// All the registers required for debug related operations.
#[derive(Debug, Clone)]
pub struct DebugRegisters(pub Vec<DebugRegister>);

impl DebugRegisters {
    /// Read all registers defined in [`RegisterFile`] from the given core.
    pub fn from_core(core: &mut Core) -> Self {
        let register_file = core.registers();

        let mut debug_registers = Vec::<DebugRegister>::new();

        let all_registers = [
            (RegisterGroup::Base, register_file.platform_registers),
            (RegisterGroup::Argument, register_file.argument_registers),
            (RegisterGroup::Result, register_file.result_registers),
            (
                RegisterGroup::Singleton,
                &[register_file.frame_pointer.to_owned()],
            ),
            (
                RegisterGroup::Singleton,
                &[register_file.program_counter.to_owned()],
            ),
            (
                RegisterGroup::Singleton,
                &[register_file.return_address.to_owned()],
            ),
            (
                RegisterGroup::Singleton,
                &[register_file.stack_pointer.to_owned()],
            ),
        ];

        for (register_group, register_group_members) in all_registers {
            for (dwarf_id, platform_register) in register_group_members.iter().enumerate() {
                // Check to ensure the register type is compatible with u64.
                if matches!(
                    platform_register.data_type(),
                    RegisterDataType::UnsignedInteger
                ) && platform_register.size_in_bits() <= 64
                {
                    if let Some(special_register) = debug_registers
                        .iter_mut()
                        .find(|debug_register| debug_register.id == platform_register.id)
                    {
                        // Some register definitions are descriptive for registers defined with the same [`RegisterId`] elsewhere, so we treat them differently.
                        special_register.special_name = Some(platform_register.name);
                    } else {
                        // It is safe for us to push a new [`DebugRegister`]
                        debug_registers.push(DebugRegister {
                            register_file,
                            group: register_group,
                            name: platform_register.name(),
                            special_name: None,
                            id: platform_register.id,
                            // TODO: Consider adding dwarf_id to RegisterDescription, to ensure we have the right values.
                            dwarf_id: if matches!(register_group, RegisterGroup::Base) {
                                Some(dwarf_id as u16)
                            } else {
                                None
                            },
                            data_type: platform_register.data_type(),
                            size_in_bits: platform_register.size_in_bits(),
                            value: match core.read_core_reg(platform_register.id) {
                                Ok::<RegisterValue, Error>(register_value) => Some(register_value),
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to read value for register {:?}: {}",
                                        platform_register,
                                        e
                                    );
                                    None
                                }
                            },
                        });
                    }
                } else {
                    tracing::warn!(
                        "Unsupported platform register type or size for register: {:?}",
                        platform_register
                    );
                }
            }
        }
        DebugRegisters(debug_registers)
    }

    /// Gets the address size for this target, in bytes
    pub fn get_address_size_bytes(&self) -> usize {
        self.get_program_counter().map_or_else(
            || 0,
            |debug_register| (debug_register.size_in_bits + 7) / 8,
            //TODO: use `div_ceil(8)` when it stabilizes
        )
    }

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn get_frame_pointer(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register.id == debug_register.register_file.frame_pointer().id
        })
    }

    /// Get the program counter.
    pub fn get_program_counter(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register.id == debug_register.register_file.program_counter().id
        })
    }

    /// Get a mutable reference to the program counter.
    pub fn get_program_counter_mut(&mut self) -> Option<&mut DebugRegister> {
        self.0.iter_mut().find(|debug_register| {
            debug_register.id == debug_register.register_file.program_counter().id
        })
    }

    /// Get the stack pointer.
    pub fn get_stack_pointer(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register.id == debug_register.register_file.stack_pointer().id
        })
    }

    /// Get the return address.
    pub fn get_return_address(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register.id == debug_register.register_file.return_address().id
        })
    }

    /// Get a register by [`RegisterId`]
    pub fn get_register(&self, register_id: RegisterId) -> Option<&DebugRegister> {
        self.0
            .iter()
            .find(|debug_register| debug_register.id == register_id)
    }

    /// Get a mutable reference register by [`RegisterId`]
    pub fn get_register_mut(&mut self, register_id: RegisterId) -> Option<&mut DebugRegister> {
        self.0
            .iter_mut()
            .find(|debug_register| debug_register.id == register_id)
    }

    /// Get the register value using the positional index into platform registers.
    /// [DWARF](https://dwarfstd.org) specification, section 2.6.1.1.3.1 "... operations encode the names of up to 32 registers, numbered from 0 through 31, inclusive ..."
    pub fn get_register_by_dwarf_id(&self, dwarf_id: u16) -> Option<&DebugRegister> {
        self.0
            .iter()
            .find(|debug_register| debug_register.dwarf_id == Some(dwarf_id))
    }

    /// Retrieve the special name if it exists, else the actual name using the [`RegisterId`] as an identifier.
    pub fn get_register_name(&self, register_id: RegisterId) -> String {
        self.0
            .iter()
            .find(|debug_register| debug_register.id == register_id)
            .map(|debug_register| debug_register.get_register_name())
            .unwrap_or_else(|| "unknown register".to_string())
    }

    /// Retrieve a register by searching against either the name or the special_name.
    pub fn get_register_by_name(&self, register_name: &str) -> Option<DebugRegister> {
        self.0
            .iter()
            .find(|&debug_register| {
                debug_register.name == register_name
                    || debug_register.special_name == Some(register_name)
            })
            .cloned()
    }
}
