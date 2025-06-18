use std::ops::Range;

use probe_rs::{
    CoreDump, CoreInterface, CoreRegister, CoreRegisters, Error, RegisterDataType, RegisterId,
    RegisterRole, RegisterValue,
};
use serde::Serialize;

/// Stores the relevant information from [`crate::core::CoreRegister`] for use in debug operations,
/// as well as additional information required during debug.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DebugRegister {
    /// To lookup platform specific details of core register definitions.
    pub core_register: &'static CoreRegister,
    /// [DWARF](https://dwarfstd.org) specification, section 2.6.1.1.3.1 "... operations encode the names of up to 32 registers, numbered from 0 through 31, inclusive ..."
    pub dwarf_id: Option<u16>,
    /// The value of the register is read from the target memory and updated as needed.
    pub value: Option<RegisterValue>,
}

impl DebugRegister {
    /// Test if this register role suggests that the value is a reference to an address in memory.
    pub(crate) fn is_pointer(&self) -> bool {
        for role in self.core_register.roles.iter() {
            if matches!(
                role,
                RegisterRole::ProgramCounter
                    | RegisterRole::FramePointer
                    | RegisterRole::StackPointer
                    | RegisterRole::ReturnAddress
                    | RegisterRole::MainStackPointer
                    | RegisterRole::ProcessStackPointer
            ) {
                return true;
            }
        }
        false
    }

    /// Return the memory range required to read the register value.
    pub fn memory_range(&self) -> Result<Option<Range<u64>>, Error> {
        if self.is_pointer() {
            if let Some(mut register_value) = self.value {
                let start_address: u64 = register_value.try_into()?;
                register_value.increment_address(self.core_register.size_in_bytes())?;
                let end_address: u64 = register_value.try_into()?;
                return Ok(Some(Range {
                    start: start_address,
                    end: end_address,
                }));
            }
        }
        Ok(None)
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
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DebugRegisters(pub Vec<DebugRegister>);

impl DebugRegisters {
    /// Read all registers defined in [`probe_rs::core::CoreRegisters`] from the given core.
    pub fn from_core(core: &mut impl CoreInterface) -> Self {
        Self::from_core_registers(core.registers(), |register_id| {
            core.read_core_reg(*register_id)
                .inspect_err(|error| {
                    tracing::warn!(
                        "Failed to read value for register {:?}: {error:#?}",
                        register_id
                    )
                })
                .ok()
        })
    }

    /// Read all registers captured in the given [`CoreDump`].
    pub fn from_coredump(core: &CoreDump) -> Self {
        Self::from_core_registers(core.registers(), |register_id| {
            let value = core.registers.get(register_id).cloned();
            if value.is_none() {
                tracing::warn!("Failed to read value for register {:?}", register_id);
            }
            value
        })
    }

    fn from_core_registers(
        regs: &'static CoreRegisters,
        mut reg_value: impl FnMut(&RegisterId) -> Option<RegisterValue>,
    ) -> Self {
        let mut debug_registers = Vec::<DebugRegister>::new();
        for (dwarf_id, core_register) in regs.core_registers().enumerate() {
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
                    value: reg_value(&core_register.id()),
                });
            } else {
                tracing::trace!(
                    "Unwind will use the default rule for this register : {:?}",
                    core_register
                );
            }
        }
        DebugRegisters(debug_registers)
    }

    /// Gets the address size for this target, in bytes
    pub fn get_address_size_bytes(&self) -> usize {
        self.get_program_counter()
            .map(|debug_register| debug_register.core_register.size_in_bits().div_ceil(8))
            .unwrap_or(0)
    }

    /// Get the canonical frame address, as specified in the [DWARF](https://dwarfstd.org) specification, section 6.4.
    /// [DWARF](https://dwarfstd.org)
    ///
    /// This is not always available
    pub fn get_frame_pointer(&self) -> Option<&DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::FramePointer)
        })
    }

    /// Get the program counter.
    pub fn get_program_counter<'b, 'c: 'b>(&'c self) -> Option<&'b DebugRegister> {
        self.0.iter().find(|debug_register| {
            debug_register
                .core_register
                .register_has_role(RegisterRole::ProgramCounter)
        })
    }

    /// Get a mutable reference to the program counter.
    pub fn get_program_counter_mut<'b, 'c: 'b>(&'c mut self) -> Option<&'b mut DebugRegister> {
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

    /// Retrieve a reference to a register by searching against an exact match of the [`RegisterRole`].
    pub fn get_register_by_role(
        &self,
        register_role: &RegisterRole,
    ) -> Result<&DebugRegister, Error> {
        let qualifying_registers = self
            .0
            .iter()
            .filter(|debug_register| {
                debug_register
                    .core_register
                    .roles
                    .iter()
                    .any(|role| role == register_role)
            })
            .collect::<Vec<&DebugRegister>>();
        if qualifying_registers.is_empty() {
            Err(Error::Register(format!(
                "No {register_role:?} registers. Please report this as a bug."
            )))
        } else if qualifying_registers.len() == 1 {
            qualifying_registers.first().cloned().ok_or_else(|| {
                Error::Register(format!(
                    "No {register_role:?} registers. Please report this as a bug."
                ))
            })
        } else {
            Err(Error::Register(format!(
                "Multiple {register_role:?} registers. Please report this as a bug."
            )))
        }
    }

    /// Retrieve the stored value of a register by searching against an exact match of the [`RegisterRole`].
    pub fn get_register_value_by_role(&self, register_role: &RegisterRole) -> Result<u64, Error> {
        self.get_register_by_role(register_role)?
            .value
            .ok_or_else(|| {
                Error::Register(format!(
                    "No value for {register_role:?} register. Please report this as a bug."
                ))
            })?
            .try_into()
    }

    /// Retrieve a mutable reference to a register by searching against an exact match of the [`RegisterRole`].
    pub fn get_register_mut_by_role(
        &mut self,
        register_role: &RegisterRole,
    ) -> Result<&mut DebugRegister, Error> {
        self.get_register_mut(self.get_register_by_role(register_role)?.core_register.id)
            .ok_or_else(|| {
                Error::Register(format!(
                    "No {register_role:?} registers. Please report this as a bug."
                ))
            })
    }

    /// Retrieve a register by searching against either the name or the role name.
    /// Use this for registers that have platform specific names like "t1", or "s9", etc.,
    /// and cannot efficiently be accessed through any of the other methods.
    pub fn get_register_by_name(&self, register_name: &str) -> Option<DebugRegister> {
        self.0
            .iter()
            .find(|&debug_register| {
                for role in debug_register.core_register.roles {
                    if matches!(role, RegisterRole::Core(role_name) | RegisterRole::Argument(role_name) | RegisterRole::Return(role_name)  | RegisterRole::Other(role_name) if *role_name == register_name) {
                        return true;
                    }
                }
                false
            })
            .cloned()
    }
}
