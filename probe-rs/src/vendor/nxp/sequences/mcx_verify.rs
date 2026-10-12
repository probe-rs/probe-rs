//! Fast flash verification for NXP MCX parts, using the FMU's MISR.
//!
//! The flash module can compute a signature over a range of flash itself (the Read into
//! MISR command), so an image can be checked without reading it back over the debug link.
//! The host runs the same signature over the data it expects and compares one 128-bit
//! value.

use std::fmt::Debug;
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::config::{MemoryRange, NvmRegion};
use crate::flashing::{FlashVerify, VerifyOutcome};
use crate::{Core, Error, MemoryInterface};

/// Flash Memory Module register block.
const FMU_BASE: u64 = 0x4009_5000;
const FSTAT: u64 = FMU_BASE;
/// FCCOB0; the rest follow at four byte intervals.
const FCCOB: u64 = FMU_BASE + 0x10;

/// Flash Memory Controller register block.
const FMC_BASE: u64 = 0x4009_4000;
/// Data Remap.
const REMAP: u64 = FMC_BASE + 0x20;
/// REMAP[LIM], the half-of-flash swap limit. Remapping is off while this is zero.
const REMAP_LIM: u32 = 0x007F_0000;

/// Where the program flash the FMU signs is mapped.
///
/// The Read into MISR command reaches program flash block 0 and, on the dual-array parts,
/// block 1; those are mapped from zero and run to at most 2 MiB. Anything else a part calls
/// flash - IFR at 0x0100_0000, a FlexSPI device at 0x0800_0000 - is either a different
/// command or a different controller, so it is not ours to sign.
const PROGRAM_FLASH: Range<u64> = 0..0x20_0000;

const CCIF: u32 = 1 << 7;
const ACCERR: u32 = 1 << 5;
const PVIOL: u32 = 1 << 4;
const CMDABT: u32 = 1 << 2;
const FAIL: u32 = 1 << 0;

/// Read into MISR.
const CMD_RDMISR: u32 = 0x05;

/// A flash page, the granularity the command starts on.
const PAGE: u64 = 128;
/// A flash phrase, the granularity the command ends on and the MISR consumes.
const PHRASE: u64 = 16;
/// The same, where a length in bytes is wanted.
const PHRASE_LEN: usize = PHRASE as usize;

/// The MISR characteristic polynomial, X^128 + X^126 + X^101 + X^99 + 1, without the X^128
/// term that the shift itself provides.
const POLY: u128 = (1u128 << 126) | (1u128 << 101) | (1u128 << 99) | 1;

/// Run the FMU's MISR over `data` in software.
///
/// One flash phrase is consumed per step: the register shifts up, feeds back through the
/// polynomial, and the phrase is mixed in.
fn misr(seed: u128, data: &[u8]) -> u128 {
    let mut state = seed;
    for phrase in data.as_chunks::<PHRASE_LEN>().0 {
        let feedback = state >> 127;
        state <<= 1;
        if feedback == 1 {
            state ^= POLY;
        }
        state ^= u128::from_le_bytes(*phrase);
    }
    state
}

/// Whether the FMU's Read into MISR command reaches `region`.
///
/// A part can carry flash the command cannot sign: IFR, which answers to Read IFR into
/// MISR instead, or a FlexSPI device, which is not the FMU's at all. Pointing a flash
/// command at either would be a command to the wrong controller over the wrong address
/// space.
fn signs(region: &NvmRegion) -> bool {
    PROGRAM_FLASH.contains_range(&region.range)
}

/// Verification backed by the FMU's Read into MISR command.
#[derive(Debug)]
pub struct McxMisrVerify;

impl FlashVerify for McxMisrVerify {
    fn verify_range(
        &self,
        core: &mut Core<'_>,
        region: &NvmRegion,
        address: u64,
        data: &[u8],
    ) -> Result<VerifyOutcome, Error> {
        if !signs(region) {
            return Ok(VerifyOutcome::Unsupported);
        }

        // The command starts on a page and ends on the last phrase of a page, so a range
        // that is not a whole number of pages cannot be signed. The caller reads those back.
        if !address.is_multiple_of(PAGE) || !(data.len() as u64).is_multiple_of(PAGE) {
            return Ok(VerifyOutcome::Unsupported);
        }
        if data.is_empty() {
            return Ok(VerifyOutcome::Match);
        }

        // The FMC can swap the two halves of flash, which the boot ROM turns on to boot a
        // second image. That swap sits between the bus and the flash, so it moved the image
        // we just programmed, but the FMU signs the arrays directly and would not see it.
        // The signature would describe the wrong pages, so leave this to the caller.
        if core.read_word_32(REMAP)? & REMAP_LIM != 0 {
            tracing::debug!("FMC is remapping flash; the FMU signature would not describe it");
            return Ok(VerifyOutcome::Unsupported);
        }

        let status = core.read_word_32(FSTAT)?;
        if status & CCIF == 0 {
            // Something else is driving the flash controller; do not interfere with it.
            return Ok(VerifyOutcome::Unsupported);
        }
        // Clear stale error flags, which would otherwise block the command.
        core.write_word_32(FSTAT, ACCERR | PVIOL | CMDABT)?;

        let end_phrase = address + data.len() as u64 - PHRASE;
        core.write_word_32(FCCOB, CMD_RDMISR)?;
        core.write_word_32(FCCOB + 0x08, address as u32)?;
        core.write_word_32(FCCOB + 0x0C, end_phrase as u32)?;
        // Seed of zero, matching what the software side starts from.
        for word in 0..4 {
            core.write_word_32(FCCOB + 0x10 + word * 4, 0)?;
        }

        // Clearing CCIF launches the command.
        core.write_word_32(FSTAT, CCIF)?;

        let started = Instant::now();
        let status = loop {
            let status = core.read_word_32(FSTAT)?;
            if status & CCIF != 0 {
                break status;
            }
            if started.elapsed() > Duration::from_secs(1) {
                return Err(Error::Timeout);
            }
        };

        // FAIL covers an uncorrectable ECC fault, which leaves no signature in FCCOB. It is a
        // status flag, so it is not in the write that clears the rest.
        if status & (ACCERR | PVIOL | CMDABT | FAIL) != 0 {
            tracing::debug!("RDMISR rejected {address:#010x}, FSTAT={status:#010x}");
            core.write_word_32(FSTAT, ACCERR | PVIOL | CMDABT)?;
            return Ok(VerifyOutcome::Unsupported);
        }

        let mut signature = 0u128;
        for word in 0..4 {
            signature |= u128::from(core.read_word_32(FCCOB + 0x10 + word * 4)?) << (32 * word);
        }

        Ok(if signature == misr(0, data) {
            VerifyOutcome::Match
        } else {
            VerifyOutcome::Mismatch
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from an MCXA266 over SWD: the FMU signed this page with a seed of zero.
    const PAGE_DATA: &str = "0080032009000000fee7ab0a404f111279236c4bcdc2dc9eabf1605ee8a6a3df\
1f479f8c46d9f478569546a97362d971b3351540abca634f9d9d6964959bd5bd\
cbccebf8af85cc9f2ff4f4387c322b4818efabb390623354e34feea9708fe2ba\
c088dea677e0c12e697511b7d52bd36a72204ca6f0212c699896155f96222f9f";
    const PAGE_SIGNATURE: u128 = 0x6661315f_f2ad0447_c5917385_aa738d6c;

    fn page_bytes() -> Vec<u8> {
        (0..PAGE_DATA.len() / 2)
            .map(|i| u8::from_str_radix(&PAGE_DATA[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn software_misr_matches_the_hardware_signature() {
        assert_eq!(misr(0, &page_bytes()), PAGE_SIGNATURE);
    }

    #[test]
    fn a_single_flipped_bit_changes_the_signature() {
        let mut data = page_bytes();
        data[64] ^= 0x01;
        assert_ne!(misr(0, &data), PAGE_SIGNATURE);
    }

    #[test]
    fn the_seed_is_carried_into_the_result() {
        let data = page_bytes();
        assert_ne!(misr(1, &data), misr(0, &data));
    }

    #[test]
    fn an_empty_range_returns_the_seed() {
        assert_eq!(misr(0x1234, &[]), 0x1234);
    }

    fn region(range: Range<u64>) -> NvmRegion {
        NvmRegion {
            name: None,
            range,
            cores: vec!["main".to_string()],
            memory_ports: vec![],
            is_alias: false,
            access: None,
        }
    }

    #[test]
    fn the_internal_program_flash_is_signed() {
        // MCXA266 and MCXA577, the small and large ends of the parts this covers.
        assert!(signs(&region(0..0xfe000)));
        assert!(signs(&region(0..0x200000)));
    }

    #[test]
    fn flash_on_another_controller_is_left_alone() {
        // A FlexSPI device, which the FMU does not reach at all.
        assert!(!signs(&region(0x8000000..0x10000000)));
        // IFR, which answers to a different command.
        assert!(!signs(&region(0x1000000..0x1010000)));
    }

    #[test]
    fn a_region_running_past_the_array_is_left_alone() {
        assert!(!signs(&region(0..0x200001)));
    }
}
