use std::{mem, ops::Range};

use probe_rs::{Core, MemoryInterface};

/// Registers stacked on exception entry.
#[derive(Debug)]
pub struct Stacked {
    r0: u32,
    r1: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    pub lr: u32,
    pub pc: u32,
    xpsr: u32,
    fpu_regs: Option<StackedFpuRegs>,
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
        let mut storage = [0; Self::WORDS_EXTENDED];
        let registers: &mut [_] = if fpu {
            &mut storage
        } else {
            &mut storage[..Self::WORDS_BASIC]
        };

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
            xpsr: registers[7],
            fpu_regs: if fpu {
                Some(StackedFpuRegs {
                    s0: f32::from_bits(registers[8]),
                    s1: f32::from_bits(registers[9]),
                    s2: f32::from_bits(registers[10]),
                    s3: f32::from_bits(registers[11]),
                    s4: f32::from_bits(registers[12]),
                    s5: f32::from_bits(registers[13]),
                    s6: f32::from_bits(registers[14]),
                    s7: f32::from_bits(registers[15]),
                    s8: f32::from_bits(registers[16]),
                    s9: f32::from_bits(registers[17]),
                    s10: f32::from_bits(registers[18]),
                    s11: f32::from_bits(registers[19]),
                    s12: f32::from_bits(registers[20]),
                    s13: f32::from_bits(registers[21]),
                    s14: f32::from_bits(registers[22]),
                    s15: f32::from_bits(registers[23]),
                    fpscr: registers[24],
                })
            } else {
                None
            },
        }))
    }

    /// Returns the in-memory size of these stacked registers, in Bytes.
    pub fn size(&self) -> u32 {
        let num_words = if self.fpu_regs.is_none() {
            Self::WORDS_BASIC
        } else {
            Self::WORDS_EXTENDED
        };

        num_words as u32 * 4
    }
}

#[derive(Debug)]
struct StackedFpuRegs {
    s0: f32,
    s1: f32,
    s2: f32,
    s3: f32,
    s4: f32,
    s5: f32,
    s6: f32,
    s7: f32,
    s8: f32,
    s9: f32,
    s10: f32,
    s11: f32,
    s12: f32,
    s13: f32,
    s14: f32,
    s15: f32,
    fpscr: u32,
}
