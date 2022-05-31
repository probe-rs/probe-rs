use probe_rs::{
    config::{CoreType, MemoryRegion},
    Core, RegisterNumber, InstructionSet,
};

/// Extension trait for probe_rs::Core, which adds some GDB -> probe-rs internal translation functions.
///
/// Translates some GDB architecture dependant stuff
/// to probe-rs internals.
pub(crate) trait GdbArchitectureExt {
    /// Translates a GDB register number to an internal register address.
    fn translate_gdb_register_number(
        &mut self,
        gdb_reg_number: u32,
    ) -> Option<(RegisterNumber, u32)>;

    /// Returns the number of general registers.
    fn num_general_registers(&mut self) -> usize;
}

impl<'probe> GdbArchitectureExt for Core<'probe> {
    fn translate_gdb_register_number(
        &mut self,
        gdb_reg_number: u32,
    ) -> Option<(RegisterNumber, u32)> {
        let (probe_rs_number, bytesize): (u16, _) = match self.architecture() {
            probe_rs::Architecture::Arm => {
                match self.instruction_set().unwrap_or(InstructionSet::Thumb2) {
                    InstructionSet::A64 => match gdb_reg_number {
                        // x0-30, SP, PC
                        x @ 0..=32 => (x as u16, 8),
                        // CPSR
                        x @ 33 => (x as u16, 4),
                        // FPSR
                        x @ 66 => (x as u16, 4),
                        // FPCR
                        x @ 67 => (x as u16, 4),
                        other => {
                            log::warn!("Request for unsupported register with number {}", other);
                            return None;
                        }
                    },
                    _ => match gdb_reg_number {
                        // Default ARM register (arm-m-profile.xml)
                        // Register 0 to 15
                        x @ 0..=15 => (x as u16, 4),
                        // CPSR register has number 16 in probe-rs
                        // See REGSEL bits, DCRSR register, ARM Reference Manual
                        25 => (16, 4),
                        // Floating Point registers (arm-m-profile-with-fpa.xml)
                        // f0 -f7 start at offset 0x40
                        // See REGSEL bits, DCRSR register, ARM Reference Manual
                        reg @ 16..=23 => ((reg as u16 - 16 + 0x40), 12),
                        // FPSCR has number 0x21 in probe-rs
                        // See REGSEL bits, DCRSR register, ARM Reference Manual
                        24 => (0x21, 4),
                        // Other registers are currently not supported,
                        // they are not listed in the xml files in GDB
                        other => {
                            log::warn!("Request for unsupported register with number {}", other);
                            return None;
                        }
                    },
                }
            }
            probe_rs::Architecture::Riscv => match gdb_reg_number {
                // general purpose registers 0 to 31
                x @ 0..=31 => {
                    let addr: RegisterNumber = self
                        .registers()
                        .get_platform_register(x as usize)
                        .expect("riscv register must exist")
                        .into();
                    (addr.0, 8)
                }
                // Program counter
                32 => {
                    let addr: RegisterNumber = self.registers().program_counter().into();
                    (addr.0, 8)
                }
                other => {
                    log::warn!("Request for unsupported register with number {}", other);
                    return None;
                }
            },
        };

        Some((RegisterNumber(probe_rs_number as u16), bytesize))
    }

    fn num_general_registers(&mut self) -> usize {
        match self.architecture() {
            probe_rs::Architecture::Arm => {
                match self.core_type() {
                    // 16 general purpose regs
                    CoreType::Armv7a => 16,
                    // When in 64 bit mode, 31 GP regs, otherwise 16
                    CoreType::Armv8a => {
                        match self.instruction_set().unwrap_or(InstructionSet::Thumb2) {
                            InstructionSet::A64 => 31,
                            _ => 16,
                        }
                    }
                    // 16 general purpose regs, 8 FP regs
                    _ => 24,
                }
            }
            probe_rs::Architecture::Riscv => 33,
        }
    }
}

/// Extension trait for probe_rs::Session, to get XML-based target description and
/// memory map.
pub trait GdbSessionExt {
    /// Memory map in GDB XML format.
    ///
    /// See https://sourceware.org/gdb/onlinedocs/gdb/Memory-Map-Format.html#Memory-Map-Format
    fn gdb_memory_map(&mut self) -> Result<String, probe_rs::Error>;

    /// Target description in GDB XML Format.
    ///
    /// See https://sourceware.org/gdb/onlinedocs/gdb/Target-Descriptions.html#Target-Descriptions
    fn target_description(&mut self) -> Result<String, probe_rs::Error>;
}

fn build_target_description(core_type: CoreType, isa: InstructionSet) -> String {
    // GDB-architectures
    //
    // - armv6-m      -> Core-M0
    // - armv7-m      -> Core-M3
    // - armv7e-m      -> Core-M4, Core-M7
    // - armv8-m.base -> Core-M23
    // - armv8-m.main -> Core-M33
    // - riscv:rv32   -> RISCV

    let architecture = match core_type {
        CoreType::Armv6m => "armv6-m",
        CoreType::Armv7a => "armv7",
        CoreType::Armv7m => "armv7",
        CoreType::Armv7em => "armv7e-m",
        CoreType::Armv8a => match isa {
            InstructionSet::A64 => "aarch64",
            _ => "armv8-a",
        },
        CoreType::Armv8m => "armv8-m.main",
        CoreType::Riscv => "riscv:rv32",
    };

    // Only target.xml is supported
    let mut target_description = r#"<?xml version="1.0"?>
        <!DOCTYPE target SYSTEM "gdb-target.dtd">
        <target version="1.0">
        "#
    .to_owned();

    target_description.push_str(&format!("<architecture>{}</architecture>", architecture));

    target_description.push_str("</target>");

    target_description
}

impl GdbSessionExt for probe_rs::Session {
    fn gdb_memory_map(&mut self) -> Result<String, probe_rs::Error> {
        let (virtual_addressing, address_size) = {
            let core = self.core(0)?;
            let address_size = core.registers().program_counter().size_in_bits();

            (
                // Cortex-A cores use virtual addressing
                matches!(core.core_type(), CoreType::Armv7a | CoreType::Armv8a),
                address_size,
            )
        };

        let mut xml_map = r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
"#.to_owned();

        if virtual_addressing {
            // GDB will not attempt to read / write anything outside the address map.
            // However, with virtual addressing any address could be valid.  As a result
            // we mark the entire address space as RAM since that's the best assumption
            // we can make.
            let region_entry = format!(
                r#"<memory type="ram" start="0x0" length="{:#x}"/>\n"#,
                match address_size {
                    32 => 0xFFFF_FFFFu64,
                    64 => 0xFFFF_FFFF_FFFF_FFFF,
                    _ => 0x0,
                }
            );

            xml_map.push_str(&region_entry);
        } else {
            for region in &self.target().memory_map {
                let region_entry = match region {
                    MemoryRegion::Ram(ram) => format!(
                        r#"<memory type="ram" start="{:#x}" length="{:#x}"/>\n"#,
                        ram.range.start,
                        ram.range.end - ram.range.start
                    ),
                    MemoryRegion::Generic(region) => format!(
                        r#"<memory type="rom" start="{:#x}" length="{:#x}"/>\n"#,
                        region.range.start,
                        region.range.end - region.range.start
                    ),
                    MemoryRegion::Nvm(region) => {
                        // TODO: Use flash with block size
                        format!(
                            r#"<memory type="rom" start="{:#x}" length="{:#x}"/>\n"#,
                            region.range.start,
                            region.range.end - region.range.start
                        )
                    }
                };

                xml_map.push_str(&region_entry);
            }
        }

        xml_map.push_str(r#"</memory-map>"#);

        Ok(xml_map)
    }

    fn target_description(&mut self) -> Result<String, probe_rs::Error> {
        // TODO: what if they're not all equal?
        let mut core = self.core(0)?;
        Ok(build_target_description(
            core.core_type(),
            core.instruction_set()?,
        ))
    }
}

#[cfg(test)]
mod test {
    use super::{build_target_description, CoreType, InstructionSet};

    #[test]
    fn test_target_description_microbit() {
        let description = build_target_description(CoreType::Armv6m, InstructionSet::Thumb2);

        insta::assert_snapshot!(description);
    }
}
