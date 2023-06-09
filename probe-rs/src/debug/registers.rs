use crate::{
    core::{Core, RegisterDataType, RegisterId, RegisterRole, RegisterValue},
    CoreRegister, Error,
};

/// Stores the relevant information from [`crate::core::CoreRegister`] for use in debug operations,
/// as well as additional information required during debug.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugRegister {
    /// To lookup platform specific details of core register definitions.
    pub core_register: &'static CoreRegister,
    /// [DWARF](https://dwarfstd.org) specification, section 2.6.1.1.3.1 "... operations encode the names of up to 32 registers, numbered from 0 through 31, inclusive ..."
    pub dwarf_id: Option<u16>,
    /// The value of the register is read from the target memory and updated as needed.
    pub value: Option<RegisterValue>,
}

impl DebugRegister {
    /// Test if this is a 32-bit unsigned integer register
    pub(crate) fn is_u32(&self) -> bool {
        self.core_register.data_type == RegisterDataType::UnsignedInteger(32)
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

    /// Retrieve the name of the [`CoreRegister`] role if it exists, else the name.
    pub fn get_register_name(&self) -> String {
        self.core_register.to_string()
    }
}

/// All the registers required for debug related operations.
#[derive(Debug, Clone)]
pub struct DebugRegisters(pub Vec<DebugRegister>);

impl DebugRegisters {
    /// Read all registers defined in [`crate::core::CoreRegisters`] from the given core.
    pub fn from_core(core: &mut Core) -> Self {
        let mut debug_registers = Vec::<DebugRegister>::new();

        for (dwarf_id, core_register) in core.registers().core_registers().enumerate() {
            // Check to ensure the register type is compatible with u64.
            if matches!(core_register.data_type(), RegisterDataType::UnsignedInteger(size_in_bits) if size_in_bits <= 64)
            {
                debug_registers.push(DebugRegister {
                    core_register,
                    // The DWARF register ID is only valid for the first 32 registers.
                    dwarf_id: if dwarf_id < 32 {
                        Some(dwarf_id as u16)
                    } else {
                        None
                    },
                    value: match core.read_core_reg(core_register.id) {
                        Ok::<RegisterValue, Error>(register_value) => Some(register_value),
                        Err(e) => {
                            tracing::warn!(
                                "Failed to read value for register {:?}: {}",
                                core_register,
                                e
                            );
                            None
                        }
                    },
                });
            } else {
                tracing::warn!(
                    "Unsupported platform register type or size for register: {:?}",
                    core_register
                );
            }
        }
        DebugRegisters(debug_registers)
    }

    /// Gets the address size for this target, in bytes
    pub fn get_address_size_bytes(&self) -> usize {
        self.get_program_counter().map_or_else(
            || 0,
            |debug_register| (debug_register.core_register.size_in_bits() + 7) / 8,
            //TODO: use `div_ceil(8)` when it stabilizes
        )
    }

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    pub fn get_frame_pointer(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::FramePointer)
        })
    }

    /// Get the program counter.
    pub fn get_program_counter(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::ProgramCounter)
        })
    }

    /// Get a mutable reference to the program counter.
    pub fn get_program_counter_mut(&mut self) -> Option<&mut DebugRegister> {
        self.0.iter_mut().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::ProgramCounter)
        })
    }

    /// Get the stack pointer.
    pub fn get_stack_pointer(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::StackPointer)
        })
    }

    /// Get the return address.
    pub fn get_return_address(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::ReturnAddress)
        })
    }

    /// Get a register by [`RegisterId`]
    pub fn get_register(&self, register_id: RegisterId) -> Option<&DebugRegister> {
        self.0
            .iter()
            .find(|debug_register| debug_register.core_register.id == register_id)
    }

    /// Get a mutable reference register by [`RegisterId`]
    pub fn get_register_mut(&mut self, register_id: RegisterId) -> Option<&mut DebugRegister> {
        self.0
            .iter_mut()
            .find(|debug_register| debug_register.core_register.id == register_id)
    }

    /// Get the register value using the positional index into core registers.
    /// [DWARF](https://dwarfstd.org) specification, section 2.6.1.1.3.1 "... operations encode the names of up to 32 registers, numbered from 0 through 31, inclusive ..."
    pub fn get_register_by_dwarf_id(&self, dwarf_id: u16) -> Option<&DebugRegister> {
        self.0
            .iter()
            .find(|debug_register| debug_register.dwarf_id == Some(dwarf_id))
    }

    /// Retrieve the role name if it exists, else the actual name using the [`RegisterId`] as an identifier.
    pub fn get_register_name(&self, register_id: RegisterId) -> String {
        self.0
            .iter()
            .find(|debug_register| debug_register.core_register.id == register_id)
            .map(|debug_register| debug_register.get_register_name())
            .unwrap_or_else(|| "unknown register".to_string())
    }

    /// Retrieve a register by searching against either the name or the role name.
    /// Use this for registers that have platform specific names like "t1", or "s9", etc.,
    /// and cannot efficiently be accessed through any of the other methods.
    // TODO: Investigate if this can leverate the function of the same name on `CoreRegisters`
    pub fn get_register_by_name(&self, register_name: &str) -> Option<DebugRegister> {
        self.0
            .iter()
            .find(|&debug_register| {
                debug_register.core_register.name == register_name || {
                    let mut register_name_matches = false;
                    for role in debug_register.core_register.roles {
                        if matches!(role, RegisterRole::Argument(role_name) | RegisterRole::Return(role_name)  | RegisterRole::Other(role_name) if *role_name == register_name) {
                            register_name_matches = true;
                            break;
                        }
                    }
                    register_name_matches
                }
            })
            .cloned()
    }
}
