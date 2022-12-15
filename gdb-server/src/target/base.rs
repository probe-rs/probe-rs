use super::desc::GdbRegisterSource;
use super::{GdbErrorExt, RuntimeTarget};
use crate::arch::{RuntimeRegId, RuntimeRegisters};

use gdbstub::common::Tid;
use gdbstub::target::ext::base::multithread::MultiThreadBase;
use gdbstub::target::ext::base::multithread::MultiThreadResumeOps;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccess;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccessOps;
use gdbstub::target::ext::thread_extra_info::ThreadExtraInfoOps;
use gdbstub::target::TargetError;
use probe_rs::{Core, Error, MemoryInterface};

impl MultiThreadBase for RuntimeTarget<'_> {
    fn read_registers(
        &mut self,
        regs: &mut RuntimeRegisters,
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.lock().unwrap();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        regs.pc = core
            .read_core_reg(core.registers().program_counter())
            .into_target_result()?;

        let mut reg_buffer = Vec::<u8>::new();

        for reg in self.target_desc.get_registers_for_main_group() {
            let bytesize = reg.size_in_bytes();
            let mut value: u128 =
                read_register_from_source(&mut core, reg.source()).into_target_result()?;

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
        let mut session = self.session.lock().unwrap();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        core.write_core_reg(core.registers().program_counter().into(), regs.pc)
            .into_target_result()?;

        let mut current_regval_offset = 0;

        for reg in self.target_desc.get_registers_for_main_group() {
            let bytesize = reg.size_in_bytes();

            let current_regval_end = current_regval_offset + bytesize;

            if current_regval_end > regs.regs.len() {
                // Supplied write general registers command argument length not valid, tell GDB
                log::error!(
                    "Unable to write register {:#?}, because supplied register value length was too short",
                    reg.source()
                );
                return Err(TargetError::Errno(22));
            }

            let str_value = &regs.regs[current_regval_offset..current_regval_end];

            let mut value = 0;
            for (exp, ch) in str_value.iter().enumerate() {
                value += (*ch as u128) << (8 * exp);
            }

            write_register_from_source(&mut core, reg.source(), value).into_target_result()?;

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
        let mut session = self.session.lock().unwrap();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        core.read(start_addr, data).into_target_result_non_fatal()
    }

    fn write_addrs(
        &mut self,
        start_addr: u64,
        data: &[u8],
        tid: Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.lock().unwrap();
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

    fn support_thread_extra_info(&mut self) -> Option<ThreadExtraInfoOps<'_, Self>> {
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
        let mut session = self.session.lock().unwrap();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        let reg = self.target_desc.get_register(reg_id.into());
        let bytesize = reg.size_in_bytes();

        let mut value: u128 =
            read_register_from_source(&mut core, reg.source()).into_target_result()?;

        for buf_entry in buf.iter_mut().take(bytesize) {
            let byte = value as u8;
            *buf_entry = byte;
            value >>= 8;
        }

        Ok(bytesize)
    }

    fn write_register(
        &mut self,
        tid: Tid,
        reg_id: RuntimeRegId,
        val: &[u8],
    ) -> gdbstub::target::TargetResult<(), Self> {
        let mut session = self.session.lock().unwrap();
        let mut core = session.core(tid.get() - 1).into_target_result()?;

        let reg = self.target_desc.get_register(reg_id.into());
        let bytesize = reg.size_in_bytes();

        let mut value = 0;

        for (exp, ch) in val.iter().enumerate().take(bytesize) {
            value += (*ch as u128) << (8 * exp);
        }

        write_register_from_source(&mut core, reg.source(), value).into_target_result()?;

        Ok(())
    }
}

fn read_register_from_source(core: &mut Core, source: GdbRegisterSource) -> Result<u128, Error> {
    match source {
        GdbRegisterSource::SingleRegister(id) => {
            let val: u128 = core.read_core_reg(id)?;

            Ok(val)
        }
        GdbRegisterSource::TwoWordRegister {
            low,
            high,
            word_size,
        } => {
            let mut val: u128 = core.read_core_reg(low)?;
            let high_val: u128 = core.read_core_reg(high)?;

            val |= high_val << word_size;

            Ok(val)
        }
    }
}

fn write_register_from_source(
    core: &mut Core,
    source: GdbRegisterSource,
    value: u128,
) -> Result<(), Error> {
    match source {
        GdbRegisterSource::SingleRegister(id) => core.write_core_reg(id, value),
        GdbRegisterSource::TwoWordRegister {
            low,
            high,
            word_size,
        } => {
            let low_word = value & ((1 << word_size) - 1);
            let high_word = value >> word_size;

            core.write_core_reg(low, low_word)?;
            core.write_core_reg(high, high_word)
        }
    }
}
