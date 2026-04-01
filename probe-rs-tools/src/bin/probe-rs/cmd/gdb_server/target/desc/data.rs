use itertools::Itertools;
use probe_rs::{CoreRegister, CoreRegisters, CoreType, InstructionSet, RegisterId, architecture};
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
    /// Register exists in GDB's layout but cannot be read from the target
    Unavailable,
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
    /// * isa - CPU instruction set
    pub fn new(core_type: CoreType, isa: InstructionSet) -> Self {
        let arch = match core_type {
            CoreType::Armv6m => "armv6-m",
            CoreType::Armv7a | CoreType::Armv7r => "armv7",
            CoreType::Armv7m => "armv7",
            CoreType::Armv7em => "armv7e-m",
            CoreType::Armv8a => match isa {
                InstructionSet::A64 => "aarch64",
                _ => "armv8-a",
            },
            CoreType::Armv8m => "armv8-m.main",
            CoreType::Riscv => "riscv:rv32",
            CoreType::Riscv64 => "riscv:rv64",
            CoreType::Xtensa => "xtensa",
            CoreType::Avr => "avr",
        };

        Self {
            arch,
            features: vec![],
            regs: vec![],
        }
    }

    /// Get a register by GDB number
    pub fn get_register(&self, num: usize) -> Option<&GdbRegister> {
        self.regs.get(num)
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
                    reg.name.to_lowercase(),
                    reg.size,
                    reg._type
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
    pub fn add_register(&mut self, reg: &CoreRegister) {
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

    /// Add a placeholder for a register that cannot be read from the target
    pub fn add_unavailable_register(&mut self, name: impl Into<String>, size: usize) {
        self.regs.push(GdbRegister {
            name: name.into(),
            size,
            _type: size_to_type(size),
            source: GdbRegisterSource::Unavailable,
        });

        self.features.last_mut().unwrap().reg_count += 1;
    }

    /// Add a collection of registers to the current GDB feature
    pub fn add_registers<'a>(&mut self, regs: impl Iterator<Item = &'a CoreRegister>) {
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
        regs: impl Iterator<Item = &'a CoreRegister>,
        name_pattern: &'static str,
        reg_type: &'static str,
    ) {
        for (i, mut reg_pair) in (&regs.chunks(2)).into_iter().enumerate() {
            let first_reg = reg_pair.next().unwrap();
            let second_reg = reg_pair.next().unwrap();

            let first_id: RegisterId = first_reg.into();
            let second_id: RegisterId = second_reg.into();

            self.regs.push(GdbRegister {
                name: format!("{name_pattern}{i}").to_owned(),
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
                new_name.clone_into(&mut reg.name);
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
        8 => "uint8",
        16 => "uint16",
        32 => "uint32",
        64 => "uint64",
        128 => "uint128",
        _ => panic!("Unsupported size: {size}"),
    }
}

pub fn build_target_description(
    regs: &CoreRegisters,
    core_type: CoreType,
    isa: InstructionSet,
) -> TargetDescription {
    let mut desc = TargetDescription::new(core_type, isa);

    // Build the main register group
    match core_type {
        CoreType::Armv6m | CoreType::Armv7em | CoreType::Armv7m | CoreType::Armv8m => {
            build_cortex_m_registers(&mut desc, regs)
        }
        CoreType::Armv7a | CoreType::Armv7r => build_cortex_a_registers(&mut desc, regs),
        CoreType::Armv8a => match isa {
            InstructionSet::A32 => build_cortex_a_registers(&mut desc, regs),
            InstructionSet::A64 => build_aarch64_registers(&mut desc, regs),
            _ => panic!("Inconsistent ISA for Armv8-a: {isa:#?}"),
        },
        CoreType::Riscv | CoreType::Riscv64 => build_riscv_registers(&mut desc, regs),
        CoreType::Xtensa => build_xtensa_registers(&mut desc, regs),
        CoreType::Avr => {
            // AVR has no debug registers — GDB target description is minimal/empty.
            desc.add_gdb_feature("org.gnu.gdb.avr.cpu");
            desc.add_registers(regs.core_registers());
        }
    };

    desc
}

fn build_riscv_registers(desc: &mut TargetDescription, regs: &CoreRegisters) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.riscv.cpu");
    desc.add_registers(regs.core_registers());
    desc.add_register(&architecture::riscv::PC);

    if regs.fpu_registers().is_some() {
        desc.add_gdb_feature("org.gnu.gdb.riscv.fpu");
        desc.add_registers(regs.fpu_registers().unwrap());
        desc.add_registers(regs.fpu_status_registers().unwrap());
    }

    desc.update_register_type("pc", "code_ptr");
}

fn build_aarch64_registers(desc: &mut TargetDescription, regs: &CoreRegisters) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.aarch64.core");
    desc.add_registers(regs.core_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    // AArch64 always has FP support
    desc.add_gdb_feature("org.gnu.gdb.aarch64.fpu");
    desc.add_registers(regs.fpu_registers().unwrap());
    desc.add_register(regs.other_by_name("Floating Point Control").unwrap());
    desc.add_register(regs.fpsr().unwrap());

    // GDB expects PSTATE to be called CPSR, even though that's the old v7 name
    desc.update_register_name("PSTATE", "CPSR");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}

fn build_cortex_a_registers(desc: &mut TargetDescription, regs: &CoreRegisters) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.arm.core");
    desc.add_registers(regs.core_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    if regs.psp().is_some() && regs.msp().is_some() {
        // Optional m-system extension
        desc.add_gdb_feature("org.gnu.gdb.arm.m-system");
        desc.add_register(regs.msp().unwrap());
        desc.add_register(regs.psp().unwrap());
    }

    if regs.fpsr().is_some() && regs.fpu_registers().is_some() {
        desc.add_gdb_feature("org.gnu.gdb.arm.vfp");
        desc.add_registers(regs.fpu_registers().unwrap());
        desc.add_register(regs.fpsr().unwrap());
    }

    // Fix up register names to match what GDB expects
    desc.update_register_name("R13", "SP");
    desc.update_register_name("R14", "LR");
    desc.update_register_name("R15", "PC");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}

fn build_cortex_m_registers(desc: &mut TargetDescription, regs: &CoreRegisters) {
    // Create the main register group
    desc.add_gdb_feature("org.gnu.gdb.arm.m-profile");
    desc.add_registers(regs.core_registers());
    if let Some(psr) = regs.psr() {
        desc.add_register(psr);
    }

    if regs.psp().is_some() && regs.msp().is_some() {
        // Optional m-system extension
        desc.add_gdb_feature("org.gnu.gdb.arm.m-system");
        desc.add_register(regs.msp().unwrap());
        desc.add_register(regs.psp().unwrap());
    }

    if regs.fpsr().is_some() && regs.fpu_registers().is_some() {
        desc.add_gdb_feature("org.gnu.gdb.arm.vfp");
        // probe-rs exposes the single word registers, s0-s31
        // GDB requires exposing the double word registers, d0-d16
        // Each d value is made up of the two consecutive s registers
        desc.add_two_word_registers(regs.fpu_registers().unwrap(), "d", "ieee_double");
        desc.add_register(regs.fpsr().unwrap());
    }

    // Fix up register names to match what GDB expects
    desc.update_register_name("R13", "SP");
    desc.update_register_name("R14", "LR");
    desc.update_register_name("R15", "PC");

    desc.update_register_type("SP", "data_ptr");
    desc.update_register_type("PC", "code_ptr");
}

fn build_xtensa_registers(desc: &mut TargetDescription, _regs: &CoreRegisters) {
    // Xtensa GDB uses a compiled-in register layout rather than XML target
    // description features. We must match the exact register order and count
    // that xtensa-*-elf-gdb expects. This layout is for ESP32-class cores
    // (contiguous register format, 64 address registers).
    //
    // RegisterId encoding used by probe-rs for Xtensa:
    //   CPU register N:     RegisterId(N)         where N = 0..15
    //   Special register N: RegisterId(0x0100 | N)
    //   Current PC:         RegisterId(0xFF00)
    //   Current PS:         RegisterId(0xFF01)
    let cpu = |n: u16| -> RegisterId { RegisterId(n) };
    let sr = |n: u16| -> RegisterId { RegisterId(0x0100 | n) };
    let pc_id = RegisterId(0xFF00);
    let ps_id = RegisterId(0xFF01);

    desc.add_gdb_feature("org.gnu.gdb.xtensa.core");

    // Register 0: pc
    desc.add_register_from_details("pc", 32, pc_id);

    // Registers 1-16: ar0-ar15 (current window, mapped from CPU a0-a15)
    for i in 0..16u16 {
        desc.add_register_from_details(format!("ar{i}"), 32, cpu(i));
    }

    // Registers 17-64: ar16-ar63 (physical regs outside current window)
    for i in 16..64 {
        desc.add_unavailable_register(format!("ar{i}"), 32);
    }

    // Registers 65-68: loop and shift
    desc.add_register_from_details("lbeg", 32, sr(0));
    desc.add_register_from_details("lend", 32, sr(1));
    desc.add_register_from_details("lcount", 32, sr(2));
    desc.add_register_from_details("sar", 32, sr(3));

    // Registers 69-70: window control
    // We report windowbase as 0 because we only have the current window's
    // registers (placed at ar0-ar15). Reporting the real windowbase would
    // cause GDB to look at ar[windowbase*4..] which are unavailable.
    desc.add_unavailable_register("windowbase", 32);
    desc.add_register_from_details("windowstart", 32, sr(73));

    // Registers 71-72: config IDs (read-only silicon config, not available)
    desc.add_unavailable_register("configid0", 32);
    desc.add_unavailable_register("configid1", 32);

    // Register 73: processor status
    desc.add_register_from_details("ps", 32, ps_id);

    // Register 74: thread pointer (user register, not a standard SR)
    desc.add_unavailable_register("threadptr", 32);

    // Register 75: boolean register file
    desc.add_register_from_details("br", 32, sr(4));

    // Register 76: conditional store compare
    desc.add_register_from_details("scompare1", 32, sr(12));

    // Registers 77-78: MAC16 accumulator
    desc.add_register_from_details("acclo", 32, sr(16));
    desc.add_register_from_details("acchi", 32, sr(17));

    // Registers 79-82: MAC16 operand registers
    desc.add_register_from_details("m0", 32, sr(32));
    desc.add_register_from_details("m1", 32, sr(33));
    desc.add_register_from_details("m2", 32, sr(34));
    desc.add_register_from_details("m3", 32, sr(35));

    // Register 83: GPIO/trace state (Espressif-specific)
    desc.add_unavailable_register("expstate", 32);

    // Registers 84-86: double-precision FPU state
    desc.add_unavailable_register("f64r_lo", 32);
    desc.add_unavailable_register("f64r_hi", 32);
    desc.add_unavailable_register("f64s", 32);

    // Registers 87-102: single-precision FPU registers
    for i in 0..16 {
        desc.add_unavailable_register(format!("f{i}"), 32);
    }

    // Registers 103-104: FPU control/status
    desc.add_unavailable_register("fcr", 32);
    desc.add_unavailable_register("fsr", 32);

    // Register 105: memory management ID
    desc.add_unavailable_register("mmid", 32);

    // Registers 106-109: debug/memory control
    desc.add_register_from_details("ibreakenable", 32, sr(96));
    desc.add_register_from_details("memctl", 32, sr(97));
    desc.add_register_from_details("atomctl", 32, sr(99));
    desc.add_register_from_details("ddr", 32, sr(104));

    // Registers 110-111: instruction breakpoint addresses
    desc.add_register_from_details("ibreaka0", 32, sr(128));
    desc.add_register_from_details("ibreaka1", 32, sr(129));

    // Registers 112-115: data breakpoint addresses and control
    desc.add_register_from_details("dbreaka0", 32, sr(144));
    desc.add_register_from_details("dbreaka1", 32, sr(145));
    desc.add_register_from_details("dbreakc0", 32, sr(160));
    desc.add_register_from_details("dbreakc1", 32, sr(161));

    // Registers 116-122: exception program counters
    for i in 1..=7u16 {
        desc.add_register_from_details(format!("epc{i}"), 32, sr(176 + i));
    }

    // Register 123: double exception program counter
    desc.add_register_from_details("depc", 32, sr(192));

    // Registers 124-129: exception processor status
    for i in 2..=7u16 {
        desc.add_register_from_details(format!("eps{i}"), 32, sr(192 + i));
    }

    // Registers 130-136: exception save registers
    for i in 1..=7u16 {
        desc.add_register_from_details(format!("excsave{i}"), 32, sr(208 + i));
    }

    // Register 137: coprocessor enable
    desc.add_register_from_details("cpenable", 32, sr(224));

    // Registers 138-139: interrupt status (both read SR 226)
    desc.add_register_from_details("interrupt", 32, sr(226));
    desc.add_register_from_details("intset", 32, sr(226));

    // Register 140: interrupt clear (write-only)
    desc.add_unavailable_register("intclear", 32);

    // Register 141: interrupt enable
    desc.add_register_from_details("intenable", 32, sr(228));

    // Registers 142-149: exception/debug state
    desc.add_register_from_details("vecbase", 32, sr(231));
    desc.add_register_from_details("exccause", 32, sr(232));
    desc.add_register_from_details("debugcause", 32, sr(233));
    desc.add_register_from_details("ccount", 32, sr(234));
    desc.add_register_from_details("prid", 32, sr(235));
    desc.add_register_from_details("icount", 32, sr(236));
    desc.add_register_from_details("icountlevel", 32, sr(237));
    desc.add_register_from_details("excvaddr", 32, sr(238));

    // Registers 150-152: cycle comparators
    desc.add_register_from_details("ccompare0", 32, sr(240));
    desc.add_register_from_details("ccompare1", 32, sr(241));
    desc.add_register_from_details("ccompare2", 32, sr(242));

    // Registers 153-156: miscellaneous
    desc.add_register_from_details("misc0", 32, sr(244));
    desc.add_register_from_details("misc1", 32, sr(245));
    desc.add_register_from_details("misc2", 32, sr(246));
    desc.add_register_from_details("misc3", 32, sr(247));

    desc.update_register_type("pc", "code_ptr");
}
