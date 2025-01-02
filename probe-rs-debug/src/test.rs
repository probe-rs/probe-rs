//! Helper functions for testing

use probe_rs::{
    architecture::{
        arm::core::registers::{
            aarch32::{
                AARCH32_CORE_REGISTERS, AARCH32_WITH_FP_16_CORE_REGISTERS,
                AARCH32_WITH_FP_32_CORE_REGISTERS,
            },
            aarch64::AARCH64_CORE_REGISTERS,
            cortex_m::{CORTEX_M_CORE_REGISTERS, CORTEX_M_WITH_FP_CORE_REGISTERS},
        },
        riscv::registers::RISCV_CORE_REGISTERS,
        xtensa::registers::XTENSA_CORE_REGISTERS,
    },
    CoreDump, CoreType, RegisterDataType,
};

use crate::{DebugRegister, DebugRegisters};

/// Read all registers defined in [`crate::core::CoreRegisters`] from the given core.
///
/// This is currently only used for testing.
pub(crate) fn debug_registers(core: &CoreDump) -> DebugRegisters {
    let reg_list = match core.core_type {
        CoreType::Armv6m => &CORTEX_M_CORE_REGISTERS,
        CoreType::Armv7a => match core.floating_point_register_count {
            Some(16) => &AARCH32_WITH_FP_16_CORE_REGISTERS,
            Some(32) => &AARCH32_WITH_FP_32_CORE_REGISTERS,
            _ => &AARCH32_CORE_REGISTERS,
        },
        CoreType::Armv7m => {
            if core.fpu_support {
                &CORTEX_M_WITH_FP_CORE_REGISTERS
            } else {
                &CORTEX_M_CORE_REGISTERS
            }
        }
        CoreType::Armv7em => {
            if core.fpu_support {
                &CORTEX_M_WITH_FP_CORE_REGISTERS
            } else {
                &CORTEX_M_CORE_REGISTERS
            }
        }
        // TODO: This can be wrong if the CPU is 32 bit. For lack of better design at the time
        // of writing this code this differentiation has been omitted.
        CoreType::Armv8a => &AARCH64_CORE_REGISTERS,
        CoreType::Armv8m => {
            if core.fpu_support {
                &CORTEX_M_WITH_FP_CORE_REGISTERS
            } else {
                &CORTEX_M_CORE_REGISTERS
            }
        }
        CoreType::Riscv => &RISCV_CORE_REGISTERS,
        CoreType::Xtensa => &XTENSA_CORE_REGISTERS,
    };

    let mut debug_registers = Vec::<DebugRegister>::new();
    for (dwarf_id, core_register) in reg_list.core_registers().enumerate() {
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
                value: match core.registers.get(&core_register.id()) {
                    Some(register_value) => Some(*register_value),
                    None => {
                        tracing::warn!("Failed to read value for register {:?}", core_register);
                        None
                    }
                },
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
