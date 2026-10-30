//! Scan chain state and TAP batch composition.

use std::fmt;

use probe_rs_target::ScanChainElement;

use super::{JtagBatch, JtagChainAccess, TapState};
use crate::probe::common::{
    bit_sequence_to_bitvec, common_sequence, extract_idcodes, extract_ir_lengths,
};
use crate::probe::queue::{BatchExecutionError, ErasedBatch};
use crate::probe::{
    BatchError, BitSequence, CommandResult, DebugProbeError, Handle, JtagCommand, Results,
};

fn take_sequence(
    results: &mut Results,
    handle: Handle<BitSequence>,
) -> Result<BitSequence, DebugProbeError> {
    results
        .take(handle)
        .map_err(|_| DebugProbeError::Other("missing JTAG capture result".into()))
}

/// Chain parameters to select a target tap within the chain.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChainParams {
    /// The TAP's position in the chain.
    pub index: usize,

    /// IR bits to shift before the TAP.
    pub irpre: usize,

    /// IR bits to shift after the TAP.
    pub irpost: usize,

    /// DR bits to shift before the TAP.
    pub drpre: usize,

    /// DR bits to shift after the TAP.
    pub drpost: usize,

    /// Length of the instruction register.
    pub irlen: usize,
}

impl ChainParams {
    /// Returns the largest IR address for this chain configuration.
    pub fn max_ir_address(&self) -> u32 {
        (1 << self.irlen) - 1
    }

    pub(crate) fn from_jtag_chain(chain: &[ScanChainElement], selected: usize) -> Option<Self> {
        let mut params = Self {
            index: selected,
            ..Default::default()
        };
        let mut found = false;
        for (index, tap) in chain.iter().enumerate() {
            let ir_len = tap.ir_len() as usize;
            if index == selected {
                params.irlen = ir_len;
                found = true;
            } else if found {
                params.irpost += ir_len;
                params.drpost += 1;
            } else {
                params.irpre += ir_len;
                params.drpre += 1;
            }
        }

        found.then_some(params)
    }
}

/// Scan chain driver built on [`JtagChainAccess`].
pub struct JtagChain<'p> {
    probe: &'p mut dyn JtagChainAccess,
}

impl fmt::Debug for JtagChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JtagChain").finish_non_exhaustive()
    }
}

impl<'p> JtagChain<'p> {
    /// Create a chain driver over `probe`.
    pub fn new(probe: &'p mut dyn JtagChainAccess) -> Self {
        Self { probe }
    }

    /// Configure padding for the TAP at `tap`.
    ///
    /// Measures the scan chain when it is not known yet.
    pub fn select(&mut self, tap: usize) -> Result<(), DebugProbeError> {
        if (*self.probe).chain_state_ref().scan_chain.is_empty() {
            self.scan_chain()?;
        }

        let chain = &(*self.probe).chain_state_ref().scan_chain;
        let Some(params) = ChainParams::from_jtag_chain(chain, tap) else {
            return Err(DebugProbeError::TargetNotFound);
        };

        tracing::debug!("Selecting JTAG TAP: {tap}");
        tracing::debug!("Setting chain params: {params:?}");

        self.probe.chain_state().chain_params = params;
        Ok(())
    }

    /// Set the expected scan chain used to validate IR lengths.
    pub fn set_expected(&mut self, chain: &[ScanChainElement]) {
        self.probe
            .chain_state()
            .expected_scan_chain
            .replace(chain.to_vec());
    }

    /// Set the scan chain without measuring it.
    pub fn set_chain(&mut self, chain: &[ScanChainElement]) {
        let state = self.probe.chain_state();
        state.scan_chain = chain.to_vec();
        state.idcodes.clear();
    }

    /// Return the current scan chain.
    pub fn chain(&mut self) -> &[ScanChainElement] {
        &(*self.probe).chain_state_ref().scan_chain
    }

    /// Return the current chain padding parameters.
    pub fn params(&mut self) -> ChainParams {
        self.probe.chain_state().chain_params
    }

    /// Measure the scan chain when it is not already set.
    pub fn scan_chain(&mut self) -> Result<&[ScanChainElement], DebugProbeError> {
        if !(*self.probe).chain_state_ref().scan_chain.is_empty() {
            let state = (*self.probe).chain_state_ref();
            return Ok(&state.scan_chain);
        }

        const MAX_CHAIN: usize = 8;

        self.tap_reset_run()?;

        self.probe.chain_state().chain_params = ChainParams::default();

        let input = [0xFF; 4 * MAX_CHAIN];
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        let dr_handle = batch.exchange(BitSequence::from_bytes(&input, input.len() * 8));
        batch.enter(TapState::RunTestIdle);
        let mut results = self.run(batch)?;
        let response = take_sequence(&mut results, dr_handle)?;

        tracing::debug!("DR: {:?}", response.as_bits());

        let idcodes = extract_idcodes(response.as_bits())?;

        tracing::info!(
            "JTAG DR scan complete, found {} TAPs. {:?}",
            idcodes.len(),
            idcodes
        );

        tracing::debug!("Scanning JTAG chain for IR lengths");

        let ones = vec![0xff; idcodes.len()];
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        let ir_handle = batch.exchange(BitSequence::from_bytes(&ones, ones.len() * 8));
        batch.enter(TapState::RunTestIdle);
        let mut results = self.run(batch)?;
        let response = take_sequence(&mut results, ir_handle)?;

        tracing::debug!("IR scan: {}", response.as_bits());

        self.tap_reset_run()?;

        let zeros_then_ones = std::iter::repeat_n(0u8, idcodes.len())
            .chain(ones.iter().copied())
            .collect::<Vec<_>>();
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        let ir_zeros_handle = batch.exchange(BitSequence::from_bytes(
            &zeros_then_ones,
            zeros_then_ones.len() * 8,
        ));
        batch.enter(TapState::RunTestIdle);
        let mut results = self.run(batch)?;
        let response_zeros = take_sequence(&mut results, ir_zeros_handle)?;

        tracing::debug!("IR scan: {}", response_zeros.as_bits());

        let response = common_sequence(response.as_bits(), response_zeros.as_bits());

        tracing::debug!("IR scan: {}", response);

        let expected = (*self.probe)
            .chain_state_ref()
            .expected_scan_chain
            .as_ref()
            .map(|chain| {
                chain
                    .iter()
                    .filter_map(|s| s.ir_len)
                    .map(|s| s as usize)
                    .collect::<Vec<usize>>()
            });
        let ir_lens = extract_ir_lengths(response, idcodes.len(), expected.as_deref())?;

        tracing::info!("Found {} TAPs on reset scan", idcodes.len());
        tracing::debug!("Detected IR lens: {:?}", ir_lens);

        let state = self.probe.chain_state();
        state.scan_chain = idcodes
            .iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| ScanChainElement {
                ir_len: Some(irlen as u8),
                name: idcode.map(|i| i.to_string()),
            })
            .collect();
        state.idcodes = idcodes
            .into_iter()
            .map(|idcode| idcode.map(u32::from))
            .collect();

        let state = (*self.probe).chain_state_ref();
        Ok(&state.scan_chain)
    }

    /// Check whether the TAP at `tap` of [`Self::chain`] is a JTAG-DP, and reset the TAPs
    /// afterwards.
    ///
    /// ADIv5 and ADIv6 give a JTAG-DP a 4-bit IR and a 35-bit DR behind DPACC. A boundary-scan
    /// TAP can also have a 4-bit IR, but its DR behind that instruction has a different length.
    /// The check shifts a marker through DPACC and measures the delay. It ends with a read of
    /// RDBUFF in the DR, so the DR update on a JTAG-DP has no side effects.
    ///
    /// Returns an error when the TAP is not in the chain, when it does not have a 4-bit IR, when
    /// the other TAPs do not behave as BYPASS, or when the probe fails.
    pub fn is_jtag_dp(&mut self, tap: usize) -> Result<bool, DebugProbeError> {
        const DPACC: u64 = 0xA;
        const DPACC_LEN: usize = 35;
        /// RnW set, A[3:2] = 0b11.
        const RDBUFF_READ: u64 = 0b111;
        const MARKER: u64 = 0x6A5C_3E19;
        const MARKER_LEN: usize = 32;

        let chain = &(*self.probe).chain_state_ref().scan_chain;
        let Some(params) = ChainParams::from_jtag_chain(chain, tap) else {
            return Err(DebugProbeError::TargetNotFound);
        };
        if params.irlen != 4 {
            return Err(DebugProbeError::Other(format!(
                "TAP {tap} has an IR length of {}, not 4",
                params.irlen
            )));
        }
        let taps = chain.len();
        let ir_len = params.irpre + params.irlen + params.irpost;

        let saved = self.probe.chain_state().chain_params;
        self.probe.chain_state().chain_params = params;

        let marker = BitSequence::from_u64(MARKER_LEN, MARKER);
        let mut batch = JtagBatch::new();

        // Put all TAPs in BYPASS. Each one delays the marker by one bit.
        batch.enter(TapState::ShiftIr);
        batch.exchange_no_capture(BitSequence::repeat(true, ir_len));
        let mut dr = marker.clone();
        dr.extend(&BitSequence::repeat(false, taps));
        batch.enter(TapState::ShiftDr);
        let bypass_handle = batch.exchange(dr);

        // Put DPACC in the TAP. The other TAPs stay in BYPASS.
        let mut dr = marker.clone();
        dr.extend(&BitSequence::repeat(false, params.drpre + params.drpost));
        dr.extend(&BitSequence::from_u64(DPACC_LEN, RDBUFF_READ));
        self.shift_ir(&mut batch, &BitSequence::from_u64(params.irlen, DPACC));
        let dpacc_handle = self.exchange_dr(&mut batch, &dr);
        self.tap_reset(&mut batch);

        let result = self.run(batch).and_then(|mut results| {
            let bypass = take_sequence(&mut results, bypass_handle)?;
            let dpacc = take_sequence(&mut results, dpacc_handle)?;
            Ok((bypass, dpacc))
        });
        self.probe.chain_state().chain_params = saved;
        let (bypass, dpacc) = result?;

        tracing::debug!("BYPASS scan: {}", bypass.as_bits());
        tracing::debug!("DPACC scan of TAP {tap}: {}", dpacc.as_bits());

        if bypass.slice(taps, MARKER_LEN) != marker {
            return Err(DebugProbeError::Other(
                "the TAPs do not behave as BYPASS".to_string(),
            ));
        }

        let delay = params.drpre + params.drpost + DPACC_LEN;
        Ok(dpacc.slice(delay, MARKER_LEN) == marker)
    }

    /// Return the IDCODEs that the scan found, with one entry for each TAP of [`Self::chain`].
    ///
    /// The entry is `None` for a TAP without an IDCODE register. The result is empty when the
    /// chain was set with [`Self::set_chain`] and not measured.
    pub fn idcodes(&mut self) -> &[Option<u32>] {
        &(*self.probe).chain_state_ref().idcodes
    }

    fn tap_reset_run(&mut self) -> Result<(), DebugProbeError> {
        let mut batch = JtagBatch::new();
        self.tap_reset(&mut batch);
        self.run(batch)?;
        Ok(())
    }

    /// Schedule an IR write with chain padding.
    pub fn shift_ir(&mut self, batch: &mut JtagBatch, ir: &BitSequence) {
        let params = self.probe.chain_state().chain_params;
        let mut data = BitSequence::repeat(true, params.irpre);
        data.extend(ir);
        data.extend(&BitSequence::repeat(true, params.irpost));
        batch.enter(TapState::ShiftIr);
        batch.exchange_no_capture(data);
    }

    /// Schedule a DR exchange with chain padding.
    pub fn exchange_dr(&mut self, batch: &mut JtagBatch, dr: &BitSequence) -> Handle<BitSequence> {
        let params = self.probe.chain_state().chain_params;
        let drpre = params.drpre;
        let dr_len = dr.len();
        let mut data = BitSequence::repeat(false, drpre);
        data.extend(dr);
        data.extend(&BitSequence::repeat(false, params.drpost));
        batch.enter(TapState::ShiftDr);
        batch
            .exchange(data)
            .map(move |captured| captured.slice(drpre, dr_len))
    }

    /// Schedule a move to Run-Test/Idle and optional idle clocks.
    ///
    /// An exchange holds the TAP in `Shift-*`, so the register does not latch
    /// until the TAP passes through `Update-*`. End a batch that exchanges with
    /// this call, unless the next batch continues the same shift.
    pub fn run_test_idle(&mut self, batch: &mut JtagBatch, cycles: u32) {
        batch.enter(TapState::RunTestIdle);
        if cycles > 0 {
            batch.clock(cycles);
        }
    }

    /// Schedule a TAP reset into Run-Test/Idle.
    pub fn tap_reset(&mut self, batch: &mut JtagBatch) {
        batch.enter(TapState::TestLogicReset);
        batch.enter(TapState::RunTestIdle);
    }

    /// Run a batch on the probe.
    pub fn run(&mut self, batch: JtagBatch) -> Result<Results, DebugProbeError> {
        self.probe
            .run_batch(&batch)
            .map_err(|error| match error.error {
                BatchError::Probe(error) => error,
                BatchError::Specific(error) => DebugProbeError::Other(error.to_string()),
            })
    }

    /// Assert target reset through the underlying probe.
    pub fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_assert()
    }

    /// Deassert target reset through the underlying probe.
    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_deassert()
    }

    /// Execute a batch of register write and DR shift commands.
    pub fn run_command_batch(
        &mut self,
        writes: &ErasedBatch<JtagCommand>,
    ) -> Result<Results, BatchExecutionError> {
        let max_ir = self.probe.chain_state_ref().chain_params.max_ir_address();
        let ir_len = self.probe.chain_state_ref().chain_params.irlen;

        let mut batch = JtagBatch::new();
        let mut capture_handles = Vec::new();

        for (idx, command) in writes.iter() {
            match command {
                JtagCommand::WriteRegister(write) => {
                    if write.inner.address > max_ir {
                        return Err(BatchExecutionError::new_from_debug_probe(
                            DebugProbeError::Other(format!(
                                "Invalid instruction register access: {}",
                                write.inner.address
                            )),
                            Results::new(),
                        ));
                    }

                    let ir = BitSequence::from_bytes(&write.inner.address.to_le_bytes(), ir_len);
                    self.shift_ir(&mut batch, &ir);
                    let handle = self.exchange_dr(&mut batch, &write.inner.data);
                    self.run_test_idle(&mut batch, write.inner.idle_cycles);
                    if idx.should_capture() {
                        capture_handles.push(handle);
                    }
                }
                JtagCommand::ShiftDr(write) => {
                    let handle = self.exchange_dr(&mut batch, &write.inner.data);
                    self.run_test_idle(&mut batch, write.inner.idle_cycles);
                    if idx.should_capture() {
                        capture_handles.push(handle);
                    }
                }
            }
        }

        let mut run_results = match self.run(batch) {
            Ok(results) => results,
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
    use std::collections::VecDeque;
    use std::fmt;

    use super::*;
    use crate::probe::{
        BatchExecutionError, CommandResult, DebugProbe, JtagChainAccess, JtagChainState, JtagOp,
        JtagProbe, WireProtocol,
    };

    struct BatchRecorder {
        exchanges: Vec<BitSequence>,
        states: Vec<TapState>,
        jtag_state: JtagChainState,
        /// For each capturing exchange in order, the bits that the chain shifts out before the
        /// shifted-in data.
        chain_bits: VecDeque<Vec<bool>>,
    }

    impl BatchRecorder {
        fn new() -> Self {
            Self {
                exchanges: Vec::new(),
                states: Vec::new(),
                jtag_state: JtagChainState::default(),
                chain_bits: VecDeque::new(),
            }
        }
    }

    impl fmt::Debug for BatchRecorder {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("BatchRecorder")
                .field("exchanges", &self.exchanges.len())
                .finish_non_exhaustive()
        }
    }

    impl DebugProbe for BatchRecorder {
        fn get_name(&self) -> &str {
            "batch recorder"
        }

        fn speed_khz(&self) -> u32 {
            0
        }

        fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
            Ok(speed_khz)
        }

        fn attach(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), crate::Error> {
            Ok(())
        }

        fn target_reset(&mut self) -> Result<(), DebugProbeError> {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset",
            })
        }

        fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset_assert",
            })
        }

        fn set_reset_line_connected(&mut self, _connected: bool) {}

        fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn select_protocol(&mut self, _protocol: WireProtocol) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn active_protocol(&self) -> Option<WireProtocol> {
            None
        }

        fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
            self
        }
    }

    impl JtagChainAccess for BatchRecorder {
        fn chain_state(&mut self) -> &mut JtagChainState {
            &mut self.jtag_state
        }

        fn chain_state_ref(&self) -> &JtagChainState {
            &self.jtag_state
        }
    }

    impl JtagProbe for BatchRecorder {
        fn run_batch(
            &mut self,
            batch: &JtagBatch,
        ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
            let mut results = Results::new();
            for (id, op) in batch.iter() {
                if let JtagOp::EnterState(state) = op {
                    self.states.push(*state);
                }
                if let JtagOp::Exchange { data, capture } = op {
                    self.exchanges.push(data.clone());
                    if *capture && id.should_capture() {
                        let byte_len = data.len().div_ceil(8);
                        let mut bytes = vec![0u8; byte_len];
                        let chain_bits = self.chain_bits.pop_front().unwrap_or_default();
                        let captured = chain_bits.into_iter().chain(data.iter());
                        for (index, bit) in captured.take(data.len()).enumerate() {
                            if bit {
                                bytes[index / 8] |= 1 << (index % 8);
                            }
                        }
                        results.push(id, CommandResult::VecU8(bytes));
                    }
                }
            }
            Ok(results)
        }
    }

    fn three_tap_chain() -> Vec<ScanChainElement> {
        vec![
            ScanChainElement {
                name: None,
                ir_len: Some(4),
            },
            ScanChainElement {
                name: None,
                ir_len: Some(4),
            },
            ScanChainElement {
                name: None,
                ir_len: Some(4),
            },
        ]
    }

    fn params_for_tap(chain: &[ScanChainElement], tap: usize) -> ChainParams {
        let mut probe = BatchRecorder::new();
        probe.jtag_state.scan_chain = chain.to_vec();
        let mut jtag_chain = JtagChain::new(&mut probe);
        jtag_chain.select(tap).unwrap();
        jtag_chain.params()
    }

    fn run_shift_ir(probe: &mut BatchRecorder, params: ChainParams) -> BitSequence {
        probe.jtag_state.chain_params = params;
        let mut chain = JtagChain::new(probe);
        let mut batch = JtagBatch::new();
        chain.shift_ir(&mut batch, &BitSequence::from_u64(4, 0b1010));
        chain.run(batch).unwrap();
        probe.exchanges.pop().unwrap()
    }

    fn run_exchange_dr(probe: &mut BatchRecorder, params: ChainParams) -> BitSequence {
        probe.jtag_state.chain_params = params;
        let mut chain = JtagChain::new(probe);
        let mut batch = JtagBatch::new();
        let handle = chain.exchange_dr(&mut batch, &BitSequence::from_u64(8, 0b1100_0011));
        let mut results = chain.run(batch).unwrap();
        results.take(handle).unwrap()
    }

    #[test]
    fn one_tap_shift_ir_has_no_padding() {
        let mut probe = BatchRecorder::new();
        let exchange = run_shift_ir(&mut probe, ChainParams::default());
        assert_eq!(exchange.len(), 4);
        assert_eq!(exchange, BitSequence::from_u64(4, 0b1010));
    }

    #[test]
    fn three_tap_first_selected_shift_ir_has_post_padding_only() {
        let params = params_for_tap(&three_tap_chain(), 0);
        let mut probe = BatchRecorder::new();
        let exchange = run_shift_ir(&mut probe, params);
        assert_eq!(params.irpre, 0);
        assert_eq!(params.irpost, 8);
        assert_eq!(params.drpre, 0);
        assert_eq!(params.drpost, 2);
        let mut expected = BitSequence::from_u64(4, 0b1010);
        expected.extend(&BitSequence::repeat(true, 8));
        assert_eq!(exchange, expected);
    }

    #[test]
    fn three_tap_last_selected_shift_ir_has_pre_padding_only() {
        let params = params_for_tap(&three_tap_chain(), 2);
        let mut probe = BatchRecorder::new();
        let exchange = run_shift_ir(&mut probe, params);
        assert_eq!(params.irpre, 8);
        assert_eq!(params.irpost, 0);
        let mut expected = BitSequence::repeat(true, 8);
        expected.extend(&BitSequence::from_u64(4, 0b1010));
        assert_eq!(exchange, expected);
    }

    #[test]
    fn three_tap_middle_selected_shift_ir_has_all_padding() {
        let params = params_for_tap(&three_tap_chain(), 1);
        let mut probe = BatchRecorder::new();
        let exchange = run_shift_ir(&mut probe, params);
        assert_eq!(params.irpre, 4);
        assert_eq!(params.irpost, 4);
        let mut expected = BitSequence::repeat(true, 4);
        expected.extend(&BitSequence::from_u64(4, 0b1010));
        expected.extend(&BitSequence::repeat(true, 4));
        assert_eq!(exchange, expected);
    }

    #[test]
    fn exchange_dr_handle_strips_padding() {
        let chain = three_tap_chain();
        let cases = [
            (
                ChainParams::default(),
                BitSequence::from_u64(8, 0b1100_0011),
            ),
            (
                params_for_tap(&chain, 0),
                BitSequence::from_u64(8, 0b1100_0011),
            ),
            (
                params_for_tap(&chain, 2),
                BitSequence::from_u64(8, 0b1100_0011),
            ),
            (
                params_for_tap(&chain, 1),
                BitSequence::from_u64(8, 0b1100_0011),
            ),
        ];

        for (params, expected) in cases {
            let mut probe = BatchRecorder::new();
            let captured = run_exchange_dr(&mut probe, params);
            assert_eq!(captured, expected);
        }
    }

    #[test]
    fn select_measures_unknown_chain() {
        let mut probe = BatchRecorder::new();
        let mut chain = JtagChain::new(&mut probe);
        _ = chain.select(0);
        assert!(!probe.exchanges.is_empty());
    }

    #[test]
    fn select_outside_chain_errors() {
        let mut probe = BatchRecorder::new();
        probe.jtag_state.scan_chain = three_tap_chain();
        let mut chain = JtagChain::new(&mut probe);
        assert!(matches!(
            chain.select(3),
            Err(DebugProbeError::TargetNotFound)
        ));
    }

    fn idcode_bits(idcode: u32) -> impl Iterator<Item = bool> {
        (0..32).map(move |bit| idcode & (1 << bit) != 0)
    }

    #[test]
    fn scan_chain_keeps_idcodes() {
        // A TAP in BYPASS, then a TAP with an IDCODE. Both have an IR length of 4.
        let ir_capture = vec![true, false, false, false, true, false, false, false];
        let mut probe = BatchRecorder::new();
        probe.chain_bits = VecDeque::from([
            std::iter::once(false)
                .chain(idcode_bits(0x4BA0_0477))
                .collect(),
            ir_capture.clone(),
            ir_capture,
        ]);

        let mut chain = JtagChain::new(&mut probe);
        let ir_lens = chain
            .scan_chain()
            .unwrap()
            .iter()
            .map(|e| e.ir_len())
            .collect::<Vec<_>>();
        assert_eq!(ir_lens, vec![4, 4]);
        assert_eq!(chain.idcodes(), [None, Some(0x4BA0_0477)]);
    }

    fn is_jtag_dp(
        chain: &[ScanChainElement],
        tap: usize,
        chain_bits: [Vec<bool>; 2],
    ) -> (Result<bool, DebugProbeError>, BatchRecorder) {
        let mut probe = BatchRecorder::new();
        probe.jtag_state.scan_chain = chain.to_vec();
        probe.chain_bits = VecDeque::from(chain_bits);
        let result = JtagChain::new(&mut probe).is_jtag_dp(tap);
        (result, probe)
    }

    #[test]
    fn is_jtag_dp_finds_dpacc_between_bypassed_taps() {
        // TAP 1 has a 35-bit DPACC, and TAPs 0 and 2 are in BYPASS.
        let (result, probe) = is_jtag_dp(&three_tap_chain(), 1, [vec![false; 3], vec![false; 37]]);
        assert!(result.unwrap());

        let ir = &probe.exchanges[2];
        assert_eq!(ir.slice(0, 4), BitSequence::repeat(true, 4));
        assert_eq!(ir.slice(4, 4), BitSequence::from_u64(4, 0xA));
        assert_eq!(ir.slice(8, 4), BitSequence::repeat(true, 4));

        // The DP holds an RDBUFF read when the DR updates.
        let dr = &probe.exchanges[3];
        assert_eq!(
            dr.slice(dr.len() - 36, 35),
            BitSequence::from_u64(35, 0b111)
        );
        assert!(matches!(
            probe.states[..],
            [.., TapState::TestLogicReset, TapState::RunTestIdle]
        ));
        assert_eq!(probe.jtag_state.chain_params.irlen, 0);
    }

    #[test]
    fn is_jtag_dp_rejects_boundary_scan_tap() {
        // DPACC of the TAP selects a 1-bit register.
        let (result, _) = is_jtag_dp(&three_tap_chain()[..1], 0, [vec![false], vec![false]]);
        assert!(!result.unwrap());
    }

    #[test]
    fn is_jtag_dp_errors_when_bypass_check_fails() {
        // The BYPASS register of a TAP is 2 bits long.
        let (result, _) = is_jtag_dp(
            &three_tap_chain()[..1],
            0,
            [vec![false; 2], vec![false; 35]],
        );
        assert!(result.is_err());
    }

    #[test]
    fn is_jtag_dp_errors_on_other_ir_length() {
        let chain = [ScanChainElement {
            name: None,
            ir_len: Some(5),
        }];
        let (result, probe) = is_jtag_dp(&chain, 0, [vec![], vec![]]);
        assert!(result.is_err());
        assert!(probe.exchanges.is_empty());
    }

    #[test]
    fn set_chain_clears_idcodes() {
        let mut probe = BatchRecorder::new();
        probe.jtag_state.idcodes = vec![Some(0x4BA0_0477)];

        let mut chain = JtagChain::new(&mut probe);
        chain.set_chain(&three_tap_chain());
        assert!(chain.idcodes().is_empty());
    }

    #[test]
    fn run_test_idle_zero_schedules_enter_run_test_idle_only() {
        let mut probe = BatchRecorder::new();
        let mut chain = JtagChain::new(&mut probe);
        let mut batch = JtagBatch::new();
        chain.run_test_idle(&mut batch, 0);
        let ops: Vec<_> = batch.iter().map(|(_, op)| op.clone()).collect();
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], JtagOp::EnterState(TapState::RunTestIdle)));
    }

    #[test]
    fn run_test_idle_nonzero_schedules_enter_and_clock() {
        let mut probe = BatchRecorder::new();
        let mut chain = JtagChain::new(&mut probe);
        let mut batch = JtagBatch::new();
        chain.run_test_idle(&mut batch, 8);
        let ops: Vec<_> = batch.iter().map(|(_, op)| op.clone()).collect();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], JtagOp::EnterState(TapState::RunTestIdle)));
        assert!(matches!(ops[1], JtagOp::ClockTck { count: 8 }));
    }
}
