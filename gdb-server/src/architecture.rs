use probe_rs::{config::MemoryRegion, Core, CoreRegisterAddress, CoreType};

/// Extension trait for probe_rs::Core, which adds some GDB -> probe-rs internal translation functions.
///
/// Translates some GDB architecture dependant stuff
/// to probe-rs internals.
pub(crate) trait GdbArchitectureExt {
    /// Translates a GDB register number to an internal register address.
    fn translate_gdb_register_number(
        &self,
        gdb_reg_number: u32,
    ) -> Option<(CoreRegisterAddress, u32)>;

    /// Returns the number of general registers.
    fn num_general_registers(&self) -> usize;
}

impl<'probe> GdbArchitectureExt for Core<'probe> {
    fn translate_gdb_register_number(
        &self,
        gdb_reg_number: u32,
    ) -> Option<(CoreRegisterAddress, u32)> {
        let (probe_rs_number, bytesize): (u16, _) = match self.architecture() {
            probe_rs::Architecture::Arm => {
                match gdb_reg_number {
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
                }
            }
            probe_rs::Architecture::Riscv => match gdb_reg_number {
                // general purpose registers 0 to 31
                x @ 0..=31 => {
                    let addr: CoreRegisterAddress = self
                        .registers()
                        .get_platform_register(x as usize)
                        .expect("riscv register must exist")
                        .into();
                    (addr.0, 8)
                }
                // Program counter
                32 => {
                    let addr: CoreRegisterAddress = self.registers().program_counter().into();
                    (addr.0, 8)
                }
                other => {
                    log::warn!("Request for unsupported register with number {}", other);
                    return None;
                }
            },
        };

        Some((CoreRegisterAddress(probe_rs_number as u16), bytesize))
    }

    fn num_general_registers(&self) -> usize {
        match self.architecture() {
            probe_rs::Architecture::Arm => 24,
            probe_rs::Architecture::Riscv => 33,
        }
    }
}

/// Extension trait for probe_rs::Target, to get XML-based target description and
/// memory map.
pub trait GdbTargetExt {
    /// Memory map in GDB XML format.
    ///
    /// See https://sourceware.org/gdb/onlinedocs/gdb/Memory-Map-Format.html#Memory-Map-Format
    fn gdb_memory_map(&self) -> String;

    /// Target description in GDB XML Format.
    ///
    /// See https://sourceware.org/gdb/onlinedocs/gdb/Target-Descriptions.html#Target-Descriptions
    fn target_description(&self) -> String;
}

impl GdbTargetExt for probe_rs::Target {
    fn gdb_memory_map(&self) -> String {
        let mut xml_map = r#"<?xml version="1.0"?>
<!DOCTYPE memory-map PUBLIC "+//IDN gnu.org//DTD GDB Memory Map V1.0//EN" "http://sourceware.org/gdb/gdb-memory-map.dtd">
<memory-map>
"#.to_owned();

        for region in &self.memory_map {
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

        xml_map.push_str(r#"</memory-map>"#);

        xml_map
    }

    fn target_description(&self) -> String {
        // GDB-architectures
        //
        // - armv6-m      -> Core-M0
        // - armv7-m      -> Core-M3
        // - armv7e-m      -> Core-M4, Core-M7
        // - armv8-m.base -> Core-M23
        // - armv8-m.main -> Core-M33
        // - riscv:rv32   -> RISCV

        let architecture = match self.core_type {
            CoreType::M0 => "armv6-m",
            CoreType::M3 => "armv7-m",
            CoreType::M4 | CoreType::M7 => "armv7e-m",
            CoreType::M33 => "armv8-m.main",
            //CoreType::M23 => "armv8-m.base",
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
}
