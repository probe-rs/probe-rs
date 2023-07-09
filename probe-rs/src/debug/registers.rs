use probe_rs_target::CoreType;

use crate::{
    core::{Core, RegisterDataType, RegisterId, RegisterRole, RegisterValue},
    CoreRegister, Error,
};

/// The rule used to preserve the value of a register between function calls duing unwinding,
/// when DWARF unwind information is not available.
/// (Applies to ARM and RISC-V). See `DebugRegister::from_core()` implementation for more details.
/// The rules for these are based on the 'Procedure Calling Standard' for each of the architectures,
/// and are documented in the `register_preserve_rule()` function.
/// Please note that the `Procedure Calling Standard` define register rules for the act of calling and/or returning from functions,
/// while the timing of a stack unwinding is different (the `callee` has not yet completed / executed the epilogue),
/// and the rules about preserving register values have to take this into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindRule {
    /// Callee-saved, a.k.a non-volatile registers, or call-preserved.
    /// If there is DWARF unwind `RegisterRule` we will apply it,
    /// otherwise we assume it was untouched and preserve the current value.
    Preserve,
    /// Caller-saved, a.k.a. volatile registers, or call-clobbered.
    /// If there is DWARF unwind `RegisterRule` we will apply it,
    /// otherwise we assume it was corrupted by the callee, and clear the value.
    Clear,
    /// Additional rules are required to determine the value of the register.
    /// These are typically found in either the DWARF unwind information,
    /// or requires additional platform specific registers to be read.
    SpecialRule,
}

/// Stores the relevant information from [`crate::core::CoreRegister`] for use in debug operations,
/// as well as additional information required during debug.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugRegister {
    /// To lookup platform specific details of core register definitions.
    pub core_register: &'static CoreRegister,
    /// For unwind purposes, we need to know how values are preserved between function calls. (Applies to ARM and RISC-V)
    pub preserve_rule: UnwindRule,
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
                    preserve_rule: register_preserve_rule(core_register, core.core_type()),
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

    /// Retrieve a mutable refererence to a register by searching against an exact match of the [`RegisterRole`].
    pub fn get_register_mut_by_role(
        &mut self,
        register_role: RegisterRole,
    ) -> Option<&mut DebugRegister> {
        self.0.iter_mut().find(|debug_register| {
            debug_register
                .core_register
                .roles
                .iter()
                .any(|&role| role == register_role)
        })
    }

    /// Retrieve a register by searching against either the name or the role name.
    /// Use this for registers that have platform specific names like "t1", or "s9", etc.,
    /// and cannot efficiently be accessed through any of the other methods.
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

    /// Update the `RegisterValue` of a register, identified by searching against either the name or the alias.
    pub fn update_register_value_by_name(
        &mut self,
        register_name: &str,
        new_value: RegisterValue,
    ) -> Result<(), Error> {
        if let Some(register) = self.0.iter_mut().find(|debug_register| {
            debug_register.core_register.name == register_name
                ||  {
                    let mut register_name_matches = false;
                    for role in debug_register.core_register.roles {
                        if matches!(role, RegisterRole::Argument(role_name) | RegisterRole::Return(role_name)  | RegisterRole::Other(role_name) if *role_name == register_name) {
                            register_name_matches = true;
                            break;
                        }
                    }
                    register_name_matches
                }
        }) {
            register.value = Some(new_value);
            Ok(())
        } else {
            Err(Error::Other(anyhow::anyhow!(format!(
                "Failed to update register {register_name}. Register not found."
            ))))
        }
    }
}

/// Determine the [`PreserveRule`] for a [`CoreRegister`], based on the [`CoreType`].
/// The rules are based on the `Procedure Calling Standard for each of the architectures.
fn register_preserve_rule(core_register: &CoreRegister, core_type: CoreType) -> UnwindRule {
    match core_type {
        // [AAPCS32](https://github.com/ARM-software/abi-aa/blob/main/aapcs32/aapcs32.rst#core-registers)
        CoreType::Armv6m | CoreType::Armv7a | CoreType::Armv7m | CoreType::Armv7em => {
            match core_register.id.0 {
                // r0-r3 are caller-saved
                0..=3 => UnwindRule::Clear,
                // r4-r8 are callee-saved
                4..=6 => UnwindRule::Preserve,
                // r7 is used by Thumb instruction set as the Frame Pointer, and is callee-saved in the ARM instruction set.
                7 => UnwindRule::SpecialRule,
                // r8 is callee-saved
                8 => UnwindRule::Preserve,
                // r9 is platform specific, so using a general rule is not possible.
                9 => UnwindRule::Clear, //SpecialRule,
                // r10-r11 are callee-saved
                10..=11 => UnwindRule::Preserve,
                // r12 is platform specific
                12 => UnwindRule::Clear, //SpecialRule,
                // r13-r14 are callee-saved
                13..=14 => UnwindRule::Preserve,
                // r15 is the program counter
                15 => UnwindRule::SpecialRule,
                // r16-r19 are callee-saved
                16..=19 => UnwindRule::Preserve,
                // r20-r31 are caller-saved
                20..=31 => UnwindRule::Clear,
                _ => unreachable!(),
            }
        }
        // [AAPCS64](https://github.com/ARM-software/abi-aa/blob/main/aapcs32/aapcs32.rst#core-registers)
        // TODO: This is a placeholder, that will allow all other core types to continue to work as before.
        CoreType::Armv8a | CoreType::Armv8m => UnwindRule::Clear,
        // [RISC-V PCS](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/releases/download/v1.0/riscv-abi.pdf)
        // TODO: This is a placeholder, that will allow all other core types to continue to work as before.
        CoreType::Riscv => UnwindRule::Clear,
    }
}
