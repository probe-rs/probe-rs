use itertools::Itertools;
use probe_rs::{CoreType, InstructionSet, RegisterDescription, RegisterFile, RegisterId};
use std::fmt::Write;

/// A feature that will be sent to GDB
struct GdbFeature {
    name: &'static str,
    reg_count: usize,
}

/// The source for a register view that will
/// be sent to GDB
#[derive(Copy, Clone, Debug)]
pub enum GdbRegisterSource {
    /// A 1:1 mapping from probe-rs register to GDB register
    SingleRegister(RegisterId),
    /// Combining two probe-rs registers into a single GDB register
    TwoWordRegister {
        low: RegisterId,
        high: RegisterId,
        word_size: usize,
    },
}

/// Information about a register sent to GDB
pub struct GdbRegister {
    name: String,
    size: usize,
    _type: &'static str,
    source: GdbRegisterSource,
}

impl GdbRegister {
    /// Size in bytes of this register
    pub fn size_in_bytes(&self) -> usize {
        self.size / 8
    }

    /// Source for this register's data
    pub fn source(&self) -> GdbRegisterSource {
        self.source
    }
}

/// A GDB target description and register info
#[derive(Default)]
pub struct TargetDescription {
    arch: &'static str,
    features: Vec<GdbFeature>,
    regs: Vec<GdbRegister>,
}

impl TargetDescription {
    /// Create a new [TargetDescription]
    ///
    /// # Arguments
    ///
    /// * core_type - CPU type
    /// * isa - CPU instruciton set
    pub fn new(core_type: CoreType, isa: InstructionSet) -> Self {
        let arch = match core_type {
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

        Self {
            arch,
            features: vec![],
            regs: vec![],
        }
    }

    /// Get a register by GDB number
    pub fn get_register(&self, num: usize) -> &GdbRegister {
        &self.regs[num]
    }

    /// Get all registers in the main feature group
    pub fn get_registers_for_main_group(&self) -> impl Iterator<Item = &GdbRegister> + '_ {
        self.regs[0..self.features[0].reg_count].iter()
    }

    /// Get the target XML to sent to GDB
    pub fn get_target_xml(&self) -> String {
        let mut target_description = r#"<?xml version="1.0"?>
        <!DOCTYPE target SYSTEM "gdb-target.dtd">
        <target version="1.0">
        "#
        .to_owned();

        let _ = write!(
            target_description,
            "<architecture>{}</architecture>",
            self.arch
        );

        let mut reg_start = 0usize;

        for feature in self.features.iter() {
            let _ = write!(target_description, "<feature name='{}'>", feature.name);

            for i in reg_start..reg_start + feature.reg_count {
                let reg = &self.regs[i];

                let _ = write!(
                    target_description,
                    "<reg name='{}' bitsize='{}' type='{}'/>",
                    reg.name, reg.size, reg._type
                );
            }

            reg_start += feature.reg_count;

            target_description.push_str("</feature>");
        }

        target_description.push_str("</target>");

        target_description
    }

    /// Add a new GDB feature
    pub fn add_gdb_feature(&mut self, name: &'static str) {
        self.features.push(GdbFeature { name, reg_count: 0 });
    }

    /// Add a register to the current GDB feature
    pub fn add_register(&mut self, reg: &RegisterDescription) {
        let id: RegisterId = reg.into();

        self.add_register_from_details(reg.name().to_owned(), reg.size_in_bits(), id);
    }

    /// Add a register to the current GDB feature
    pub fn add_register_from_details(
        &mut self,
        name: impl Into<String>,
        size: usize,
        id: RegisterId,
    ) {
        self.regs.push(GdbRegister {
            name: name.into(),
            size,
            _type: size_to_type(size),
            source: GdbRegisterSource::SingleRegister(id),
        });

        self.features.last_mut().unwrap().reg_count += 1;
    }

    /// Add a collection of registers to the current GDB feature
    pub fn add_registers<'a>(&mut self, regs: impl Iterator<Item = &'a RegisterDescription>) {
        for reg in regs {
            self.add_register(reg);
        }
    }

    /// Add a collection of registers that take pairs of probe-rs values
    /// and merge them into a single GDB view
    ///
    /// For example - s0,s1,s2,s3 becomes d0(s0,s1), d1(s2,s3)
    pub fn add_two_word_registers<'a>(
        &mut self,
        regs: impl Iterator<Item = &'a RegisterDescription>,
        name_pattern: &'static str,
        reg_type: &'static str,
    ) {
        for (i, mut reg_pair) in (&regs.chunks(2)).into_iter().enumerate() {
            let first_reg = reg_pair.next().unwrap();
            let second_reg = reg_pair.next().unwrap();

            let first_id: RegisterId = first_reg.into();
            let second_id: RegisterId = second_reg.into();

            self.regs.push(GdbRegister {
                name: format!("{}{}", name_pattern, i).to_owned(),
                size: first_reg.size_in_bits() * 2,
                _type: reg_type,
                source: GdbRegisterSource::TwoWordRegister {
                    low: first_id,
                    high: second_id,
                    word_size: first_reg.size_in_bits(),
                },
            });

            self.features.last_mut().unwrap().reg_count += 1;
        }
    }

    /// Update a register name
    pub fn update_register_name(&mut self, old_name: &'static str, new_name: &'static str) {
        for reg in self.regs.iter_mut() {
            if reg.name == old_name {
                reg.name = new_name.to_owned();
            }
        }
    }

    /// Update a register type
    pub fn update_register_type(&mut self, name: &'static str, new_type: &'static str) {
        for reg in self.regs.iter_mut() {
            if reg.name == name {
                reg._type = new_type;
            }
        }
    }
}

fn size_to_type(size: usize) -> &'static str {
    match size {
        32 => "uint32",
        64 => "uint64",
        128 => "uint128",
        _ => panic!("Unsupported size: {}", size),
    }
}

pub fn build_target_description(
    regs: &RegisterFile,
    core_type: CoreType,
    isa: InstructionSet,
) -> TargetDescription {
    let mut desc = TargetDescription::new(core_type, isa);

    // Build the main register group
    match core_type {
        CoreType::Armv6m | CoreType::Armv7em | CoreType::Armv7m | CoreType::Armv8m => {
            build_cortex_m_registers(&mut desc, regs)
        }
        CoreType::Armv7a => build_cortex_a_registers(&mut desc, regs),
        CoreType::Armv8a => match isa {
            InstructionSet::A32 => build_cortex_a_registers(&mut desc, regs),
            InstructionSet::A64 => build_aarch64_registers(&mut desc, regs),
            _ => panic!("Inconsistent ISA for Armv8-a: {:#?}", isa),
        },
        CoreType::Riscv => build_riscv_registers(&mut desc, regs),
    };

    desc
}

fn build_riscv_registers(desc: &mut TargetDescription, regs: &RegisterFile) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.riscv.cpu");
    desc.add_registers(regs.platform_registers());
    desc.add_register(regs.program_counter());

    desc.update_register_type("pc", "code_ptr");
}

fn build_aarch64_registers(desc: &mut TargetDescription, regs: &RegisterFile) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.aarch64.core");
    desc.add_registers(regs.platform_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    // AArch64 always has FP support
    desc.add_gdb_feature("org.gnu.gdb.aarch64.fpu");
    desc.add_registers(regs.fpu_registers().unwrap());
    desc.add_register(regs.other_by_name("FPCR").unwrap());
    desc.add_register(regs.fpscr().unwrap());

    // GDB expects PSTATE to be called CPSR, even though that's the old v7 name
    desc.update_register_name("PSTATE", "CPSR");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}

fn build_cortex_a_registers(desc: &mut TargetDescription, regs: &RegisterFile) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.arm.core");
    desc.add_registers(regs.platform_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    if regs.psp().is_some() && regs.msp().is_some() {
        // Optional m-system extension
        desc.add_gdb_feature("org.gnu.gdb.arm.m-system");
        desc.add_register(regs.msp().unwrap());
        desc.add_register(regs.psp().unwrap());
    }

    if regs.fpscr().is_some() && regs.fpu_registers().is_some() {
        desc.add_gdb_feature("org.gnu.gdb.arm.vfp");
        desc.add_registers(regs.fpu_registers().unwrap());
        desc.add_register(regs.fpscr().unwrap());
    }

    // Fix up register names to match what GDB expects
    desc.update_register_name("R13", "SP");
    desc.update_register_name("R14", "LR");
    desc.update_register_name("R15", "PC");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}

fn build_cortex_m_registers(desc: &mut TargetDescription, regs: &RegisterFile) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.arm.m-profile");
    desc.add_registers(regs.platform_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    if regs.psp().is_some() && regs.msp().is_some() {
        // Optional m-system extension
        desc.add_gdb_feature("org.gnu.gdb.arm.m-system");
        desc.add_register(regs.msp().unwrap());
        desc.add_register(regs.psp().unwrap());
    }

    if regs.fpscr().is_some() && regs.fpu_registers().is_some() {
        desc.add_gdb_feature("org.gnu.gdb.arm.vfp");
        // probe-rs exposes the single word registers, s0-s31
        // GDB requires exposing the double word registers, d0-d16
        // Each d value is made up of the two consecutive s registers
        desc.add_two_word_registers(regs.fpu_registers().unwrap(), "d", "ieee_double");
        desc.add_register(regs.fpscr().unwrap());
    }

    // Fix up register names to match what GDB expects
    desc.update_register_name("R13", "SP");
    desc.update_register_name("R14", "LR");
    desc.update_register_name("R15", "PC");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}
