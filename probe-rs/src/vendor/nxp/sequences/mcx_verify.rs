//! Fast flash verification for NXP MCX parts, using the FMU's MISR.
//!
//! The flash module can compute a signature over a range of flash itself (the Read into
//! MISR command), so an image can be checked without reading it back over the debug link.
//! The host runs the same signature over the data it expects and compares one 128-bit
//! value.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use crate::flashing::{FlashVerify, VerifyOutcome};
use crate::{Core, Error, MemoryInterface};

/// Flash Memory Module register block.
const FMU_BASE: u64 = 0x4009_5000;
const FSTAT: u64 = FMU_BASE;
/// FCCOB0; the rest follow at four byte intervals.
const FCCOB: u64 = FMU_BASE + 0x10;

const CCIF: u32 = 1 << 7;
const ACCERR: u32 = 1 << 5;
const PVIOL: u32 = 1 << 4;
const CMDABT: u32 = 1 << 2;

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

/// Verification backed by the FMU's Read into MISR command.
#[derive(Debug)]
pub struct McxMisrVerify;

impl FlashVerify for McxMisrVerify {
    fn verify_range(
        &self,
        core: &mut Core<'_>,
        address: u64,
        data: &[u8],
    ) -> Result<VerifyOutcome, Error> {
        // The command starts on a page and ends on the last phrase of a page, so a range
        // that is not a whole number of pages cannot be signed. The caller reads those back.
        if !address.is_multiple_of(PAGE) || !(data.len() as u64).is_multiple_of(PAGE) {
            return Ok(VerifyOutcome::Unsupported);
        }
        if data.is_empty() {
            return Ok(VerifyOutcome::Match);
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

        if status & (ACCERR | PVIOL | CMDABT) != 0 {
            // The controller rejected the range rather than disagreeing about its contents.
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
}
