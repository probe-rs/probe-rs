use crate::TIMEOUT;
use crate::{Elf, TargetInfo};
use probe_rs::{MemoryInterface, Session};

const CANARY_VALUE: u8 = 0xAA;

/// (Location of) the stack canary
///
/// The stack canary is used to detect *potential* stack overflows
///
/// The canary is placed in memory as shown in the diagram below:
///
/// ``` text
/// +--------+ -> initial_stack_pointer
/// |        |
/// | stack  | (grows downwards)
/// |        |
/// +--------+
/// |        |
/// |        |
/// +--------+
/// | canary |
/// +--------+ -> highest_static_var_address
/// |        |
/// | static | (variables, fixed size)
/// |        |
/// +--------+ -> lowest RAM address
/// ```
///
/// The whole canary is initialized to `CANARY_VALUE` before the target program is started.
/// The canary size is 10% of the available stack space or 1 KiB, whichever is smallest.
///
/// When the programs ends (due to panic or breakpoint) the integrity canary is checked. If it was
/// "touched" (any of its bytes != `CANARY_VALUE`) then that is considered to be a *potential* stack
/// overflow
///
/// The canary is not installed if the program memory layout is "inverted" (stack is *below* the
/// static variables)
#[derive(Clone, Copy)]
pub(crate) struct Canary {
    address: u32,
    size: usize,
}

impl Canary {
    pub(crate) fn install(
        sess: &mut Session,
        target_info: &TargetInfo,
        elf: &Elf,
    ) -> Result<Option<Self>, anyhow::Error> {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

        // Decide if and where to place the stack canary.
        if let Some(highest_static_var_address) = target_info.highest_static_var_address {
            // standard = static variables are at a lower address; stack (grows down) is at a higher address
            let standard_memory_layout =
                highest_static_var_address < elf.vector_table.initial_stack_pointer;

            if !elf.target_program_uses_heap() && standard_memory_layout {
                let stack_available =
                    elf.vector_table.initial_stack_pointer - highest_static_var_address - 1;

                // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb since
                // filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're doing).
                let size = 1024.min(stack_available / 10) as usize;

                log::debug!(
                    "{} bytes of stack available ({:#010X} ..= {:#010X}), using {} byte canary to detect overflows",
                    stack_available,
                    highest_static_var_address + 1,
                    elf.vector_table.initial_stack_pointer,
                    size,
                );

                // Canary starts right after `highest_ram_addr_in_use`.
                let address = highest_static_var_address + 1;
                let canary = vec![CANARY_VALUE; size];
                core.write_8(address, &canary)?;

                return Ok(Some(Canary { address, size }));
            }
        }

        Ok(None)
    }

    pub(crate) fn touched(self, core: &mut probe_rs::Core, elf: &Elf) -> anyhow::Result<bool> {
        let mut canary = vec![0; self.size];
        core.read_8(self.address, &mut canary)?;

        if let Some(pos) = canary.iter().position(|b| *b != CANARY_VALUE) {
            let touched_address = self.address + pos as u32;
            log::debug!("canary was touched at {:#010X}", touched_address);

            let min_stack_usage = elf.vector_table.initial_stack_pointer - touched_address;
            log::warn!(
                "program has used at least {} bytes of stack space, data segments \
                     may be corrupted due to stack overflow",
                min_stack_usage,
            );
            Ok(true)
        } else {
            log::debug!("stack canary intact");
            Ok(false)
        }
    }
}
