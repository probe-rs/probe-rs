use crate::TIMEOUT;
use crate::{cortexm, ProcessedElf, TargetInfo};
use anyhow::bail;
use probe_rs::{MemoryInterface, Session};
use std::time::Duration;

const STACK_CANARY: u8 = 0xAA;

pub(crate) struct Canary {
    address: u32,
    size: usize,
}

pub(crate) fn touched(
    canary: Option<Canary>,
    core: &mut probe_rs::Core,
    elf: &ProcessedElf,
) -> anyhow::Result<bool> {
    if let Some(canary) = canary {
        let mut buf = vec![0; canary.size];
        core.read_8(canary.address, &mut buf)?;

        if let Some(pos) = buf.iter().position(|b| *b != STACK_CANARY) {
            let touched_addr = canary.address + pos as u32;
            log::debug!("canary was touched at 0x{:08X}", touched_addr);

            let min_stack_usage = elf.vector_table.initial_stack_pointer - touched_addr;
            log::warn!(
                "program has used at least {} bytes of stack space, data segments \
                may be corrupted due to stack overflow",
                min_stack_usage,
            );
            return Ok(true);
        } else {
            log::debug!("stack canary intact");
        }
    }

    Ok(false)
}

pub(crate) fn place(
    sess: &mut Session,
    target_info: &TargetInfo,
    elf: &ProcessedElf,
) -> Result<Option<Canary>, anyhow::Error> {
    let mut canary = None;
    {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

        // Decide if and where to place the stack canary.
        if let (Some(ram), Some(highest_ram_addr_in_use)) = (
            &target_info.active_ram_region,
            target_info.highest_ram_addr_in_use,
        ) {
            // Initial SP must be past canary location.
            let initial_sp_makes_sense = ram
                .range
                .contains(&(elf.vector_table.initial_stack_pointer - 1))
                && highest_ram_addr_in_use < elf.vector_table.initial_stack_pointer;
            if target_info.highest_ram_addr_in_use.is_some()
                && !elf.target_program_uses_heap
                && initial_sp_makes_sense
            {
                let stack_available =
                    elf.vector_table.initial_stack_pointer - highest_ram_addr_in_use - 1;

                // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb since
                // filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're doing).
                let canary_size = 1024.min(stack_available / 10) as usize;

                log::debug!(
                    "{} bytes of stack available (0x{:08X}-0x{:08X}), using {} byte canary to detect overflows",
                    stack_available,
                    highest_ram_addr_in_use + 1,
                    elf.vector_table.initial_stack_pointer,
                    canary_size,
                );

                // Canary starts right after `highest_ram_addr_in_use`.
                let canary_addr = highest_ram_addr_in_use + 1;
                canary = Some(Canary {
                    address: canary_addr,
                    size: canary_size,
                });
                let data = vec![STACK_CANARY; canary_size];
                core.write_8(canary_addr, &data)?;
            }
        }

        log::debug!("starting device");
        if core.get_available_breakpoint_units()? == 0 {
            if elf.rtt_buffer_address.is_some() {
                bail!("RTT not supported on device without HW breakpoints");
            } else {
                log::warn!("device doesn't support HW breakpoints; HardFault will NOT make `probe-run` exit with an error code");
            }
        }

        if let Some(rtt) = elf.rtt_buffer_address {
            core.set_hw_breakpoint(elf.main_function_address)?;
            core.run()?;
            core.wait_for_core_halted(Duration::from_secs(5))?;
            const OFFSET: u32 = 44;
            const FLAG: u32 = 2; // BLOCK_IF_FULL
            core.write_word_32(rtt + OFFSET, FLAG)?;
            core.clear_hw_breakpoint(elf.main_function_address)?;
        }

        core.set_hw_breakpoint(cortexm::clear_thumb_bit(elf.vector_table.hard_fault))?;
        core.run()?;
    }
    Ok(canary)
}
