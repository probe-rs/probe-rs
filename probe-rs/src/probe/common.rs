//! Crate-public structures and utilities to be shared between probes.

use bitfield::bitfield;
use bitvec::prelude::*;
use probe_rs_target::ScanChainElement;

use crate::probe::{
    BitSequence, CommandResult, DebugProbeError, JtagAccess, JtagBatch, JtagChainAccess,
    JtagCommand, JtagProbe, JtagSequence, TapState,
    jtag::chain::JtagChain,
    queue::{BatchExecutionError, ErasedBatch, Results},
};
use bitvec::vec::BitVec;

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
/// Because we don't know how many TAPs there are, we scan until we find
/// a 32-bit IDCODE of all 1s, which comes after the last TAP in the chain.
///
/// Returns `Vec<Option<IdCode>>`, with None for TAPs in BYPASS.
pub(crate) fn extract_idcodes<T: BitStore>(
    mut dr: &BitSlice<T>,
) -> Result<Vec<Option<IdCode>>, ScanChainError> {
    let mut idcodes = Vec::new();
    let mut accumulated_bypass_taps = 0;

    while !dr.is_empty() {
        if dr[0] {
            if dr.len() < 32 {
                tracing::error!("Truncated IDCODE: {dr:02X?}");
                return Err(ScanChainError::InvalidIdCode);
            }

            let idcode = dr[0..32].load_le::<u32>();

            if idcode == u32::MAX {
                break;
            }

            let idcode = IdCode(idcode);

            if !idcode.valid() {
                tracing::error!("Invalid IDCODE: {:08X}", idcode.0);
                return Err(ScanChainError::InvalidIdCode);
            }

            if accumulated_bypass_taps != 0 {
                tracing::info!("Appending {accumulated_bypass_taps} bypass taps");
                for _ in 0..accumulated_bypass_taps {
                    idcodes.push(None);
                }
                accumulated_bypass_taps = 0;
            }
            tracing::info!("Found IDCODE: {idcode}");
            idcodes.push(Some(idcode));
            dr = &dr[32..];
        } else {
            accumulated_bypass_taps += 1;
            dr = &dr[1..];
        }
    }
    Ok(idcodes)
}

pub(crate) fn common_sequence<'a, S: BitStore>(
    a: &'a BitSlice<S>,
    b: &BitSlice<S>,
) -> &'a BitSlice<S> {
    let common_length = a.iter().zip(b.iter()).take_while(|(a, b)| *a == *b).count();

    &a[..common_length]
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
/// <https://github.com/GlasgowEmbedded/glasgow/blob/30dc11b2/software/glasgow/applet/interface/jtag_probe/__init__.py#L712>
///
/// Returns `Vec<usize>`, with an entry for each TAP.
pub(crate) fn extract_ir_lengths<T: BitStore>(
    ir: &BitSlice<T>,
    n_taps: usize,
    expected: Option<&[usize]>,
) -> Result<Vec<usize>, ScanChainError> {
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
        if n_taps < starts.len() {
            // We have more possible starts than TAPs. This may be because some devices start the
            // IR scan with 101xx. Try to merge length 2 IRs with their neighbours.
            let mut irlens = starts_to_lengths(&starts, ir.len()).into_iter();
            let mut merged = Vec::new();
            while let Some(len) = irlens.next() {
                if len == 2
                    && let Some(next) = irlens.next()
                {
                    merged.push(len + next);
                    continue;
                }
                merged.push(len);
            }

            // Only succeed if we end up with the expected number of IRs.
            if merged.len() == n_taps {
                tracing::info!("IR lengths after merging 101xx prefixes: {merged:?}");
                return Ok(merged);
            }
        }

        tracing::error!("IR lengths are ambiguous and must be explicitly configured.");
        Err(ScanChainError::InvalidIR)
    }
}

fn with_jtag_chain<P, R>(probe: &mut P, f: impl FnOnce(&mut JtagChain<'_>) -> R) -> R
where
    P: JtagChainAccess,
{
    let mut chain = JtagChain::new(probe);
    f(&mut chain)
}

fn bit_sequence_to_bitvec(sequence: &BitSequence) -> BitVec {
    let mut bits = BitVec::new();
    bits.extend_from_bitslice(sequence.as_bits());
    bits
}

impl<Probe: JtagChainAccess> JtagAccess for Probe {
    fn shift_raw_sequence(&mut self, sequence: JtagSequence) -> Result<BitVec, DebugProbeError> {
        JtagProbe::shift_raw_sequence(self, sequence)
    }

    fn enter_tap_state(&mut self, state: TapState) -> Result<(), DebugProbeError> {
        with_jtag_chain(self, |chain| {
            let mut batch = JtagBatch::new();
            batch.enter(state);
            chain.run(batch).map(|_| ())
        })
    }

    fn set_expected_scan_chain(
        &mut self,
        scan_chain: &[ScanChainElement],
    ) -> Result<(), DebugProbeError> {
        with_jtag_chain(self, |chain| chain.set_expected(scan_chain));
        Ok(())
    }

    fn set_scan_chain(&mut self, scan_chain: &[ScanChainElement]) -> Result<(), DebugProbeError> {
        with_jtag_chain(self, |chain| chain.set_chain(scan_chain));
        Ok(())
    }

    /// Configures the probe to address the given target.
    fn select_target(&mut self, target: usize) -> Result<(), DebugProbeError> {
        if self.chain_state_ref().scan_chain.is_empty() {
            self.scan_chain()?;
        }

        with_jtag_chain(self, |chain| chain.select(target))
    }

    fn scan_chain(&mut self) -> Result<&[ScanChainElement], DebugProbeError> {
        with_jtag_chain(self, |chain| chain.scan_chain().map(|_| ()))?;
        Ok(self.chain_state_ref().scan_chain.as_slice())
    }

    fn tap_reset(&mut self) -> Result<(), DebugProbeError> {
        with_jtag_chain(self, |chain| {
            let mut batch = JtagBatch::new();
            chain.tap_reset(&mut batch);
            chain.run(batch).map(|_| ())
        })
    }

    fn read_register(
        &mut self,
        address: u32,
        len: u32,
        idle_cycles: u32,
    ) -> Result<BitVec, DebugProbeError> {
        let data = vec![0u8; len.div_ceil(8) as usize];

        self.write_register(address, &data, len, idle_cycles)
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
        idle_cycles: u32,
    ) -> Result<BitVec, DebugProbeError> {
        if address > self.chain_state_ref().chain_params.max_ir_address() {
            return Err(DebugProbeError::Other(format!(
                "Invalid instruction register access: {address}"
            )));
        }

        let ir_len = self.chain_state_ref().chain_params.irlen;

        let response = with_jtag_chain(self, |chain| {
            let mut batch = JtagBatch::new();
            let ir = BitSequence::from_bytes(&address.to_le_bytes(), ir_len);
            chain.shift_ir(&mut batch, &ir);
            let handle =
                chain.exchange_dr(&mut batch, &BitSequence::from_bytes(data, len as usize));
            chain.run_test_idle(&mut batch, idle_cycles);
            let mut results = chain.run(batch)?;
            results
                .take(handle)
                .map_err(|_| DebugProbeError::Other("missing JTAG capture result".into()))
        })?;

        tracing::trace!("receive_write_dr result: {:?}", response.as_bits());
        Ok(bit_sequence_to_bitvec(&response))
    }

    fn write_dr(
        &mut self,
        data: &[u8],
        len: u32,
        idle_cycles: u32,
    ) -> Result<BitVec, DebugProbeError> {
        let response = with_jtag_chain(self, |chain| {
            let mut batch = JtagBatch::new();
            let handle =
                chain.exchange_dr(&mut batch, &BitSequence::from_bytes(data, len as usize));
            chain.run_test_idle(&mut batch, idle_cycles);
            let mut results = chain.run(batch)?;
            results
                .take(handle)
                .map_err(|_| DebugProbeError::Other("missing JTAG capture result".into()))
        })?;

        tracing::trace!("write_dr result: {:?}", response.as_bits());
        Ok(bit_sequence_to_bitvec(&response))
    }

    #[tracing::instrument(skip(self, writes))]
    fn write_register_batch(
        &mut self,
        writes: &ErasedBatch<JtagCommand>,
    ) -> Result<Results, BatchExecutionError> {
        let max_ir = self.chain_state_ref().chain_params.max_ir_address();

        let (mut run_results, capture_handles) = match with_jtag_chain(self, |chain| {
            let ir_len = chain.params().irlen;
            let mut batch = JtagBatch::new();
            let mut capture_handles = Vec::new();

            for (idx, command) in writes.iter() {
                match command {
                    JtagCommand::WriteRegister(write) => {
                        if write.inner.address > max_ir {
                            return Err(DebugProbeError::Other(format!(
                                "Invalid instruction register access: {}",
                                write.inner.address
                            )));
                        }

                        let ir =
                            BitSequence::from_bytes(&write.inner.address.to_le_bytes(), ir_len);
                        chain.shift_ir(&mut batch, &ir);
                        let handle = chain.exchange_dr(
                            &mut batch,
                            &BitSequence::from_bytes(&write.inner.data, write.inner.len as usize),
                        );
                        chain.run_test_idle(&mut batch, write.inner.idle_cycles);
                        if idx.should_capture() {
                            capture_handles.push(handle);
                        }
                    }
                    JtagCommand::ShiftDr(write) => {
                        let handle = chain.exchange_dr(
                            &mut batch,
                            &BitSequence::from_bytes(&write.inner.data, write.inner.len as usize),
                        );
                        chain.run_test_idle(&mut batch, write.inner.idle_cycles);
                        if idx.should_capture() {
                            capture_handles.push(handle);
                        }
                    }
                }
            }

            let run_results = chain.run(batch)?;
            Ok((run_results, capture_handles))
        }) {
            Ok(value) => value,
            Err(error) => {
                return Err(BatchExecutionError::new_from_debug_probe(
                    error,
                    Results::new(),
                ));
            }
        };

        tracing::debug!("Got responses! Processing...");
        let mut responses = Results::with_capacity(writes.len());
        let mut capture_handles = capture_handles.into_iter();

        for (idx, command) in writes.iter() {
            if idx.should_capture() {
                let Some(handle) = capture_handles.next() else {
                    return Err(BatchExecutionError::new_from_debug_probe(
                        DebugProbeError::Other("missing batch capture handle".into()),
                        responses,
                    ));
                };

                let response = match run_results.take(handle) {
                    Ok(response) => response,
                    Err(_) => {
                        return Err(BatchExecutionError::new_from_debug_probe(
                            DebugProbeError::Other("missing batch capture result".into()),
                            responses,
                        ));
                    }
                };
                let response = bit_sequence_to_bitvec(&response);

                let result = match command {
                    JtagCommand::WriteRegister(cmd) => {
                        (cmd.transform)(&cmd.inner, response.as_bitslice())
                    }
                    JtagCommand::ShiftDr(cmd) => {
                        (cmd.transform)(&cmd.inner, response.as_bitslice())
                    }
                };

                match result {
                    Ok(response) => responses.push(idx, response),
                    Err(e) => return Err(BatchExecutionError::new_specific(e, responses)),
                }
            } else {
                responses.push(idx, CommandResult::None);
            }
        }

        Ok(responses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARM_TAP: IdCode = IdCode(0x4BA00477);
    const STM_BS_TAP: IdCode = IdCode(0x06433041);

    #[test]
    fn id_code_display() {
        let debug_fmt = format!("{ARM_TAP}");
        assert_eq!(debug_fmt, "0x4BA00477 (ARM Ltd)");

        let debug_fmt = format!("{STM_BS_TAP}");
        assert_eq!(debug_fmt, "0x06433041 (STMicroelectronics)");
    }

    #[test]
    fn extract_ir_lengths_with_one_tap() {
        let ir = bits![1, 0, 0, 0];
        let n_taps = 1;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4]);
    }

    #[test]
    fn extract_ir_lengths_with_two_taps() {
        // The STM32F1xx and STM32F4xx are examples of MCUs that two serially connected JTAG TAPs,
        // the boundary scan TAP (IR is 5-bit wide) and the Cortex® -M4 with FPU TAP (IR is 4-bit wide).
        // This test ensures our scan chain interrogation handles this scenario.
        let ir = bits![1, 0, 0, 0, 1, 0, 0, 0, 0];
        let n_taps = 2;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4, 5]);
    }

    #[test]
    fn extract_ir_lengths_with_two_taps_101() {
        // Slightly contrived example where the IR scan starts with 101xx. In known real devices
        // the 101 TAP is 5 bits long, but this is an edge case that the algorithm should handle.
        let ir = bits![1, 0, 1, 0, 1, 0, 0, 0, 0];
        let n_taps = 2;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4, 5]);
    }

    #[test]
    fn extract_id_codes_one_tap() {
        let dr = bits![mut 0; 32];
        dr[0..32].store_le(ARM_TAP.0);

        let idcodes = extract_idcodes(dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP)]);
    }

    #[test]
    fn extract_id_codes_two_taps() {
        let dr = bits![mut 0; 64];
        dr[0..32].store_le(ARM_TAP.0);
        dr[32..64].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), Some(STM_BS_TAP)]);
    }

    #[test]
    fn extract_id_codes_tap_bypass_tap() {
        let dr = bits![mut 0; 65];
        dr[0..32].store_le(ARM_TAP.0);
        dr.set(32, false);
        dr[33..65].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), None, Some(STM_BS_TAP)]);
    }
}
