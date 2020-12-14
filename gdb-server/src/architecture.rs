use probe_rs::{Core, CoreRegisterAddress};

/// Extension trait for probe_rs::Core, which adds some GDB -> probe-rs internal translation functions
///
/// translates some GDB architecture dependant stuff
/// to probe-rs internals.
pub(crate) trait GdbArchitectureExt {
    /// Translate GDB register number to internal register address
    fn translate_gdb_register_number(
        &self,
        gdb_reg_number: u32,
    ) -> Option<(CoreRegisterAddress, u32)>;

    /// Number of general registers
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
