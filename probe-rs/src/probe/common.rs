//! Crate-public structures and utilities to be shared between probes.

use bitfield::bitfield;
use bitvec::prelude::*;

pub(crate) fn bits_to_byte(bits: impl IntoIterator<Item = bool>) -> u32 {
    let mut bit_val = 0u32;

    for (index, bit) in bits.into_iter().take(32).enumerate() {
        if bit {
            bit_val |= 1 << index;
        }
    }

    bit_val
}

bitfield! {
    /// A JTAG IDCODE.
    /// Identifies a particular Test Access Port (TAP) on the JTAG scan chain.
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct IdCode(u32);
    impl Debug;

    u8;
    /// The IDCODE version.
    pub version, set_version: 31, 28;

    u16;
    /// The part number.
    pub part_number, set_part_number: 27, 12;

    /// The JEDEC JEP-106 Manufacturer ID.
    pub manufacturer, set_manufacturer: 11, 1;

    u8;
    /// The continuation code of the JEDEC JEP-106 Manufacturer ID.
    pub manufacturer_continuation, set_manufacturer_continuation: 11, 8;

    /// The identity code of the JEDEC JEP-106 Manufacturer ID.
    pub manufacturer_identity, set_manufacturer_identity: 7, 1;

    bool;
    /// The least-significant bit.
    /// Always set.
    pub lsbit, set_lsbit: 0;
}

impl std::fmt::Display for IdCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(mfn) = self.manufacturer_name() {
            write!(f, "0x{:08X} ({})", self.0, mfn)
        } else {
            write!(f, "0x{:08X}", self.0)
        }
    }
}

impl IdCode {
    /// Returns `true` iff the IDCODE's least significant bit is `1`
    /// and the 7-bit `manufacturer_identity` is set to one of the non-reserved values in the range `[1,126]`.
    pub fn valid(&self) -> bool {
        self.lsbit() && (self.manufacturer() != 0) && (self.manufacturer() != 127)
    }

    /// Return the manufacturer name, if available.
    pub fn manufacturer_name(&self) -> Option<&'static str> {
        let cc = self.manufacturer_continuation();
        let id = self.manufacturer_identity();
        jep106::JEP106Code::new(cc, id).get()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanChainError {
    #[error("Invalid IDCODE")]
    InvalidIdCode,
    #[error("Invalid IR scan chain")]
    InvalidIR,
}

/// Convert a list of start positions to a list of lengths.
fn starts_to_lengths(starts: &[usize], total: usize) -> Vec<usize> {
    let mut lens: Vec<usize> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    lens.push(total - lens.iter().sum::<usize>());
    lens
}

/// Extract all IDCODEs from a test-logic-reset DR chain `dr`.
///
/// Valid IDCODEs have a '1' in the least significant (first) bit,
/// and are 32 bits long. DRs in BYPASS always have a single 0 bit.
///
/// We can therefore unambiguously scan through the DR capture to find
/// all IDCODEs and TAPs in BYPASS.
///
/// Returns Vec<Option<IdCode>>, with None for TAPs in BYPASS.
pub(crate) fn extract_idcodes(
    mut dr: &BitSlice<u8>,
) -> Result<Vec<Option<IdCode>>, ScanChainError> {
    let mut idcodes = Vec::new();

    while !dr.is_empty() {
        if dr[0] {
            if dr.len() < 32 {
                tracing::error!("Truncated IDCODE: {dr:02X?}");
                return Err(ScanChainError::InvalidIdCode);
            }

            let idcode = dr[0..32].load_le::<u32>();
            let idcode = IdCode(idcode);

            if !idcode.valid() {
                tracing::error!("Invalid IDCODE: {:08X}", idcode.0);
                return Err(ScanChainError::InvalidIdCode);
            }
            tracing::info!("Found IDCODE: {idcode}");
            idcodes.push(Some(idcode));
            dr = &dr[32..];
        } else {
            idcodes.push(None);
            tracing::info!("Found bypass TAP");
            dr = &dr[1..];
        }
    }
    Ok(idcodes)
}

/// Best-effort extraction of IR lengths from a test-logic-reset IR chain `ir`,
/// which is known to contain `n_taps` TAPs (as discovered by scanning DR for IDCODEs).
///
/// If expected IR lengths are provided, specify them in `expected`, and they are
/// verified against the IR scan and then returned.
///
/// Valid IRs in the capture must start with `0b10` (a 1 in the least-significant,
/// and therefore first, bit). However, IRs may contain `0b10` in other positions, so we
/// can only find a superset of all possible start positions. If this happens to match
/// the number of taps, or there is only one tap, we can find all IR lengths. Otherwise,
/// they must be provided, and are then checked.
///
/// This implementation is a port of the algorithm from:
/// https://github.com/GlasgowEmbedded/glasgow/blob/30dc11b2/software/glasgow/applet/interface/jtag_probe/__init__.py#L712
///
/// Returns Vec<usize>, with an entry for each TAP.
pub(crate) fn extract_ir_lengths(
    ir_ones: &BitSlice<u8>,
    ir_zeros: &BitSlice<u8>,
    n_taps: usize,
    expected: Option<&[usize]>,
) -> Result<Vec<usize>, ScanChainError> {
    let common_length = ir_ones
        .iter()
        .zip(ir_zeros.iter())
        .take_while(|(a, b)| *a == *b)
        .count();

    let ir = &ir_ones[..common_length];

    // Find all `10` patterns which indicate potential IR start positions.
    let starts = ir
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] && !w[1])
        .map(|(i, _)| i)
        .collect::<Vec<usize>>();
    tracing::trace!("Possible IR start positions: {starts:?}");

    if n_taps == 0 {
        tracing::error!("Cannot scan IR without at least one TAP");
        Err(ScanChainError::InvalidIR)
    } else if n_taps > starts.len() {
        // We must have at least as many `10` patterns as TAPs.
        tracing::error!("Fewer IRs detected than TAPs");
        Err(ScanChainError::InvalidIR)
    } else if starts[0] != 0 {
        // The chain must begin with a possible start location.
        tracing::error!("IR chain does not begin with a valid start pattern");
        Err(ScanChainError::InvalidIR)
    } else if let Some(expected) = expected {
        // If expected lengths are available, verify and return them.
        if expected.len() != n_taps {
            tracing::error!(
                "Number of provided IR lengths ({}) does not match \
                         number of detected TAPs ({n_taps})",
                expected.len()
            );

            Err(ScanChainError::InvalidIR)
        } else if expected.iter().sum::<usize>() != ir.len() {
            tracing::error!(
                "Sum of provided IR lengths ({}) does not match \
                         length of IR scan ({} bits)",
                expected.iter().sum::<usize>(),
                ir.len()
            );
            Err(ScanChainError::InvalidIR)
        } else {
            let exp_starts = expected
                .iter()
                .scan(0, |a, &x| {
                    let b = *a;
                    *a += x;
                    Some(b)
                })
                .collect::<Vec<usize>>();
            tracing::trace!("Provided IR start positions: {exp_starts:?}");
            let unsupported = exp_starts.iter().filter(|s| !starts.contains(s)).count();
            if unsupported > 0 {
                tracing::error!(
                    "Provided IR lengths imply an IR start position \
                             which is not supported by the IR scan"
                );
                Err(ScanChainError::InvalidIR)
            } else {
                tracing::debug!("Verified provided IR lengths against IR scan");
                Ok(starts_to_lengths(&exp_starts, ir.len()))
            }
        }
    } else if n_taps == 1 {
        // If there's only one TAP, this is easy.
        tracing::info!("Only one TAP detected, IR length {}", ir.len());
        Ok(vec![ir.len()])
    } else if n_taps == starts.len() {
        // If the number of possible starts matches the number of TAPs,
        // we can unambiguously find all lengths.
        let irlens = starts_to_lengths(&starts, ir.len());
        tracing::info!("IR lengths are unambiguous: {irlens:?}");
        Ok(irlens)
    } else {
        tracing::error!("IR lengths are ambiguous and must be explicitly configured.");
        Err(ScanChainError::InvalidIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARM_TAP: IdCode = IdCode(0x4BA00477);
    const STM_BS_TAP: IdCode = IdCode(0x06433041);

    #[test]
    fn id_code_display() {
        let debug_fmt = format!("{idcode}", idcode = ARM_TAP);
        assert_eq!(debug_fmt, "0x4BA00477 (ARM Ltd)");

        let debug_fmt = format!("{idcode}", idcode = STM_BS_TAP);
        assert_eq!(debug_fmt, "0x06433041 (STMicroelectronics)");
    }

    #[test]
    fn extract_ir_lengths_with_one_tap() {
        let ir = &bitvec![u8, Lsb0; 1,0,0,0];
        let n_taps = 1;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4]);
    }

    #[test]
    fn extract_ir_lengths_with_two_taps() {
        // The STM32F1xx and STM32F4xx are examples of MCUs that two serially connected JTAG TAPs,
        // the boundary scan TAP (IR is 5-bit wide) and the Cortex® -M4 with FPU TAP (IR is 4-bit wide).
        // This test ensures our scan chain interrogation handles this scenario.
        let ir = &bitvec![u8, Lsb0; 1,0,0,0,1,0,0,0,0];
        let n_taps = 2;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4, 5]);
    }

    #[test]
    fn extract_id_codes_one_tap() {
        let mut dr = bitvec![u8, Lsb0; 0; 32];
        dr[0..32].store_le(ARM_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP)]);
    }

    #[test]
    fn extract_id_codes_two_taps() {
        let mut dr = bitvec![u8, Lsb0; 0; 64];
        dr[0..32].store_le(ARM_TAP.0);
        dr[32..64].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), Some(STM_BS_TAP)]);
    }

    #[test]
    fn extract_id_codes_tap_bypass_tap() {
        let mut dr = bitvec![u8, Lsb0; 0; 65];
        dr[0..32].store_le(ARM_TAP.0);
        dr.set(32, false);
        dr[33..65].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), None, Some(STM_BS_TAP)]);
    }
}
