use std::{mem, ops::Range};

use probe_rs::{Core, MemoryInterface};

/// Registers stacked on exception entry.
#[derive(Debug)]
pub(crate) struct Stacked {
    r0: u32,
    r1: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    pub lr: u32,
    pub pc: u32,
    contains_fpu_regs: bool,
}

fn bounds_check(bounds: Range<u32>, start: u32, len: u32) -> Result<(), ()> {
    let end = start + len;
    if bounds.contains(&start) && bounds.contains(&end) {
        Ok(())
    } else {
        Err(())
    }
}

impl Stacked {
    /// Minimum number of stacked registers that we need to read to be able to unwind an exception
    const WORDS_MINIMUM: usize = 7;

    /// Number of 32-bit words stacked in a basic frame.
    const WORDS_BASIC: usize = 8;

    /// Number of 32-bit words stacked in an extended frame.
    const WORDS_EXTENDED: usize = Self::WORDS_BASIC + 17; // 16 FPU regs + 1 status word

    /// Reads stacked registers from RAM
    ///
    /// This performs bound checks and returns `None` if a invalid memory read is requested
    pub fn read(
        core: &mut Core<'_>,
        sp: u32,
        fpu: bool,
        ram_bounds: Range<u32>,
    ) -> anyhow::Result<Option<Self>> {
        let mut storage = [0; Self::WORDS_MINIMUM];
        let registers: &mut [_] = &mut storage;

        if bounds_check(
            ram_bounds,
            sp,
            (registers.len() * mem::size_of::<u32>()) as u32,
        )
        .is_err()
        {
            return Ok(None);
        }

        core.read_32(sp, registers)?;

        Ok(Some(Stacked {
            r0: registers[0],
            r1: registers[1],
            r2: registers[2],
            r3: registers[3],
            r12: registers[4],
            lr: registers[5],
            pc: registers[6],
            contains_fpu_regs: fpu,
        }))
    }

    /// Returns the in-memory size of these stacked registers, in Bytes.
    pub fn size(&self) -> u32 {
        let num_words = if self.contains_fpu_regs {
            Self::WORDS_EXTENDED
        } else {
            Self::WORDS_BASIC
        };

        num_words as u32 * 4
    }
}
