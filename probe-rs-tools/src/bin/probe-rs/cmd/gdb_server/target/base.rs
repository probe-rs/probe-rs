use super::{GdbErrorExt, RuntimeTarget};
use crate::cmd::gdb_server::arch::{RuntimeRegId, RuntimeRegisters};
use crate::cmd::gdb_server::target::desc::GdbRegisterSource;
use crate::rpc::functions::core_ops::convert::{
    from_wire_register_value, to_wire_register_id, to_wire_register_value,
};

use gdbstub::common::Tid;
use gdbstub::target::ext::base::multithread::MultiThreadBase;
use gdbstub::target::ext::base::multithread::MultiThreadResumeOps;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccess;
use gdbstub::target::ext::base::single_register_access::SingleRegisterAccessOps;
use gdbstub::target::ext::thread_extra_info::ThreadExtraInfoOps;
use gdbstub::target::{TargetError, TargetResult};
use probe_rs::RegisterValue;
use probe_rs_rpc::core_ops::WireRegisterId;
use probe_rs_rpc_client::ClientError;

impl MultiThreadBase for RuntimeTarget {
    fn read_registers(&mut self, regs: &mut RuntimeRegisters, tid: Tid) -> TargetResult<(), Self> {
        let core_index = tid.get() - 1;
        let core = self.core(core_index);
        let registers = self.core_cache(core_index)?.registers;

        let pc_id = registers
            .pc()
            .ok_or_else(|| TargetError::Fatal(anyhow::anyhow!("Core has no program counter")))?
            .id();
        let pc_value = self
            .block_on(core.read_core_reg(to_wire_register_id(pc_id)))
            .into_target_result()?;
        regs.pc = register_value_to_u64(from_wire_register_value(pc_value))?;

        let mut reg_buffer = Vec::<u8>::new();

        for reg in self.target_desc.get_registers_for_main_group() {
            let bytesize = reg.size_in_bytes();
            let mut value: u128 =
                read_register_from_source(self, core_index, reg.source()).into_target_result()?;

            for _ in 0..bytesize {
                reg_buffer.push(value as u8);
                value >>= 8;
            }
        }

        regs.regs = reg_buffer;

        Ok(())
    }

    fn write_registers(&mut self, regs: &RuntimeRegisters, tid: Tid) -> TargetResult<(), Self> {
        let core_index = tid.get() - 1;
        let core = self.core(core_index);
        let registers = self.core_cache(core_index)?.registers;

        let pc_id = registers
            .pc()
            .ok_or_else(|| TargetError::Fatal(anyhow::anyhow!("Core has no program counter")))?
            .id();
        self.block_on(core.write_core_reg(
            to_wire_register_id(pc_id),
            to_wire_register_value(RegisterValue::from(regs.pc)),
        ))
        .into_target_result()?;

        let mut current_regval_offset = 0;

        for reg in self.target_desc.get_registers_for_main_group() {
            let bytesize = reg.size_in_bytes();
            let current_regval_end = current_regval_offset + bytesize;

            if current_regval_end > regs.regs.len() {
                tracing::error!(
                    "Unable to write register {:#?}, because supplied register value length was too short",
                    reg.source()
                );
                return Err(TargetError::Errno(22));
            }

            let str_value = &regs.regs[current_regval_offset..current_regval_end];
            let mut value = 0u128;
            for (exp, ch) in str_value.iter().enumerate() {
                value += (*ch as u128) << (8 * exp);
            }

            write_register_from_source(self, core_index, reg.source(), value)
                .into_target_result()?;

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
    ) -> TargetResult<usize, Self> {
        if start_addr.checked_add(data.len() as u64).is_none() {
            return Err(TargetError::Errno(14));
        }

        let core = self.core(tid.get() - 1);
        let bytes = self
            .block_on(core.read_bytes(start_addr, data.len()))
            .into_target_result_non_fatal()?;

        let num_read = bytes.len().min(data.len());
        data[..num_read].copy_from_slice(&bytes[..num_read]);
        if num_read != data.len() {
            return Err(TargetError::Errno(122));
        }

        Ok(num_read)
    }

    fn write_addrs(&mut self, start_addr: u64, data: &[u8], tid: Tid) -> TargetResult<(), Self> {
        let core = self.core(tid.get() - 1);
        self.block_on(core.write_memory_8(start_addr, data.to_vec()))
            .into_target_result_non_fatal()
    }

    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(Tid),
    ) -> Result<(), Self::Error> {
        for core in &self.cores {
            let tid = Tid::new(core.index + 1).unwrap();
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

impl SingleRegisterAccess<Tid> for RuntimeTarget {
    fn read_register(
        &mut self,
        tid: Tid,
        reg_id: RuntimeRegId,
        buf: &mut [u8],
    ) -> TargetResult<usize, Self> {
        let Some(reg) = self.target_desc.get_register(reg_id.into()) else {
            return Err(TargetError::Errno(0));
        };

        let bytesize = reg.size_in_bytes();
        let mut value: u128 =
            read_register_from_source(self, tid.get() - 1, reg.source()).into_target_result()?;

        for buf_entry in buf.iter_mut().take(bytesize) {
            *buf_entry = value as u8;
            value >>= 8;
        }

        Ok(bytesize)
    }

    fn write_register(
        &mut self,
        tid: Tid,
        reg_id: RuntimeRegId,
        val: &[u8],
    ) -> TargetResult<(), Self> {
        let Some(reg) = self.target_desc.get_register(reg_id.into()) else {
            return Err(TargetError::Errno(0));
        };

        let bytesize = reg.size_in_bytes();
        let mut value = 0u128;
        for (exp, ch) in val.iter().enumerate().take(bytesize) {
            value += (*ch as u128) << (8 * exp);
        }

        write_register_from_source(self, tid.get() - 1, reg.source(), value).into_target_result()
    }
}

impl RuntimeTarget {
    fn core_cache(&self, index: usize) -> Result<&super::CoreCache, TargetError<anyhow::Error>> {
        self.cores
            .iter()
            .find(|c| c.index == index)
            .ok_or_else(|| TargetError::Fatal(anyhow::anyhow!("Unknown core {index}")))
    }
}

fn read_register_from_source(
    target: &RuntimeTarget,
    core_index: usize,
    source: GdbRegisterSource,
) -> Result<u128, ClientError> {
    let core = target.core(core_index);
    match source {
        GdbRegisterSource::SingleRegister(id) => {
            let value = target.block_on(core.read_core_reg(WireRegisterId(id.0)))?;
            Ok(register_value_to_u128(from_wire_register_value(value)))
        }
        GdbRegisterSource::TwoWordRegister {
            low,
            high,
            word_size,
        } => {
            let low_val = target.block_on(core.read_core_reg(WireRegisterId(low.0)))?;
            let high_val = target.block_on(core.read_core_reg(WireRegisterId(high.0)))?;
            let mut val = register_value_to_u128(from_wire_register_value(low_val));
            let high_val = register_value_to_u128(from_wire_register_value(high_val));
            val |= high_val << word_size;
            Ok(val)
        }
        GdbRegisterSource::Unavailable => Ok(0),
    }
}

fn write_register_from_source(
    target: &RuntimeTarget,
    core_index: usize,
    source: GdbRegisterSource,
    value: u128,
) -> Result<(), ClientError> {
    let core = target.core(core_index);
    match source {
        GdbRegisterSource::SingleRegister(id) => target.block_on(core.write_core_reg(
            WireRegisterId(id.0),
            to_wire_register_value(register_value_from_u128(value)),
        )),
        GdbRegisterSource::TwoWordRegister {
            low,
            high,
            word_size,
        } => {
            let low_word = value & ((1 << word_size) - 1);
            let high_word = value >> word_size;
            target.block_on(core.write_core_reg(
                WireRegisterId(low.0),
                to_wire_register_value(register_value_from_u128(low_word)),
            ))?;
            target.block_on(core.write_core_reg(
                WireRegisterId(high.0),
                to_wire_register_value(register_value_from_u128(high_word)),
            ))
        }
        GdbRegisterSource::Unavailable => Ok(()),
    }
}

fn register_value_to_u128(value: RegisterValue) -> u128 {
    match value {
        RegisterValue::U32(v) => v as u128,
        RegisterValue::U64(v) => v as u128,
        RegisterValue::U128(v) => v,
    }
}

fn register_value_to_u64(value: RegisterValue) -> Result<u64, TargetError<anyhow::Error>> {
    value
        .try_into()
        .map_err(|e| TargetError::Fatal(anyhow::anyhow!("{e:?}")))
}

fn register_value_from_u128(value: u128) -> RegisterValue {
    if value <= u32::MAX as u128 {
        RegisterValue::U32(value as u32)
    } else if value <= u64::MAX as u128 {
        RegisterValue::U64(value as u64)
    } else {
        RegisterValue::U128(value)
    }
}
