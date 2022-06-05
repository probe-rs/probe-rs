use super::{GdbErrorExt, RuntimeTarget};
use crate::arch::{RuntimeRegId, RuntimeRegisters};

use gdbstub::common::Tid;
use gdbstub::target::ext::base::multithread::MultiThreadBase;
use gdbstub::target::ext::base::multithread::MultiThreadResumeOps;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccess;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccessOps;
use gdbstub::target::TargetError;
use probe_rs::{Core, CoreType, InstructionSet, MemoryInterface, RegisterId};

impl MultiThreadBase for RuntimeTarget<'_> {
    fn read_registers(
        &mut self,
        regs: &mut RuntimeRegisters,
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        regs.pc = core
            .read_core_reg(core.registers().program_counter())
            .into_target_result()?;

        let mut reg_buffer = Vec::<u8>::new();

        for reg in 0..num_general_registers(&mut core) {
            let (probe_rs_number, bytesize) =
                translate_gdb_register_number(&mut core, reg as u32).unwrap();

            let mut value: u64 = core.read_core_reg(probe_rs_number).unwrap();

            for _ in 0..bytesize {
                let byte = value as u8;
                reg_buffer.push(byte);
                value >>= 8;
            }
        }

        regs.regs = reg_buffer;

        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &RuntimeRegisters,
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        core.write_core_reg(core.registers().program_counter().into(), regs.pc)
            .into_target_result()?;

        let mut current_regval_offset = 0;

        for reg_num in 0..num_general_registers(&mut core) as u32 {
            let (addr, bytesize) = translate_gdb_register_number(&mut core, reg_num).unwrap();

            let current_regval_end = current_regval_offset + bytesize as usize;

            if current_regval_end > regs.regs.len() {
                // Supplied write general registers command argument length not valid, tell GDB
                log::error!(
                    "Unable to write register {}, because supplied register value length was too short",
                    reg_num
                );
                return Err(TargetError::Errno(22));
            }

            let str_value = &regs.regs[current_regval_offset..current_regval_end];

            let mut value = 0;
            for (exp, ch) in str_value.iter().enumerate() {
                value += (*ch as u64) << (8 * exp);
            }

            core.write_core_reg(addr, value).into_target_result()?;

            current_regval_offset = current_regval_end;

            if current_regval_offset == regs.regs.len() {
                break;
            }
        }

        Ok(())
    }

    fn read_addrs(
        &mut self,
        start_addr: u64,
        data: &mut [u8],
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        core.read(start_addr, data).into_target_result_non_fatal()
    }

    fn write_addrs(
        &mut self,
        start_addr: u64,
        data: &[u8],
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        core.write_8(start_addr, data)
            .into_target_result_non_fatal()
    }

    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(Tid),
    ) -> Result<(), Self::Error> {
        for i in &self.cores {
            // Unwrap is always safe because we'll never pass 0 to new
            let tid = Tid::new(i + 1).unwrap();
            thread_is_active(tid);
        }

        Ok(())
    }

    fn support_resume(&mut self) -> Option<MultiThreadResumeOps<'_, Self>> {
        Some(self)
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, Tid, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<Tid> for RuntimeTarget<'_> {
    fn read_register(
        &mut self,
        tid: Tid,
        reg_id: RuntimeRegId,
        buf: &mut [u8],
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        let (probe_rs_number, bytesize) =
            translate_gdb_register_number(&mut core, reg_id.into()).unwrap();

        let mut value: u64 = core.read_core_reg(probe_rs_number).unwrap();

        for i in 0..bytesize {
            let byte = value as u8;
            buf[i as usize] = byte;
            value >>= 8;
        }

        Ok(bytesize as usize)
    }

    fn write_register(
        &mut self,
        tid: Tid,
        reg_id: RuntimeRegId,
        val: &[u8],
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.borrow_mut();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        let (probe_rs_number, bytesize) =
            translate_gdb_register_number(&mut core, reg_id.into()).unwrap();

        let mut value = 0;

        for (exp, ch) in val.iter().enumerate().take(bytesize as usize) {
            value += (*ch as u64) << (8 * exp);
        }

        core.write_core_reg(probe_rs_number, value)
            .into_target_result()?;

        Ok(())
    }
}

/// Take a GDB register number and transmate it into a Probe-RS register number
/// for use with [Core::read_core_reg()] and [Core::write_core_reg()]
fn translate_gdb_register_number(
    core: &mut Core,
    gdb_reg_number: u32,
) -> Option<(RegisterId, u32)> {
    let (probe_rs_number, bytesize): (u16, _) = match core.architecture() {
        probe_rs::Architecture::Arm => {
            match core.instruction_set().unwrap_or(InstructionSet::Thumb2) {
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
                let addr: RegisterId = core
                    .registers()
                    .get_platform_register(x as usize)
                    .expect("riscv register must exist")
                    .into();
                (addr.0, 8)
            }
            // Program counter
            32 => {
                let addr: RegisterId = core.registers().program_counter().into();
                (addr.0, 8)
            }
            other => {
                log::warn!("Request for unsupported register with number {}", other);
                return None;
            }
        },
    };

    Some((RegisterId(probe_rs_number as u16), bytesize))
}

fn num_general_registers(core: &mut Core) -> usize {
    match core.architecture() {
        probe_rs::Architecture::Arm => {
            match core.core_type() {
                // 16 general purpose regs
                CoreType::Armv7a => 16,
                // When in 64 bit mode, 31 GP regs, otherwise 16
                CoreType::Armv8a => {
                    match core.instruction_set().unwrap_or(InstructionSet::Thumb2) {
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
