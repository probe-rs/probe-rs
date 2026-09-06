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

    fn from_jtag_chain(chain: &[ScanChainElement], selected: usize) -> Option<Self> {
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
    pub fn select(&mut self, tap: usize) -> Result<(), DebugProbeError> {
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
        self.probe.chain_state().scan_chain = chain.to_vec();
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

        self.probe.chain_state().scan_chain = idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| ScanChainElement {
                ir_len: Some(irlen as u8),
                name: idcode.map(|i| i.to_string()),
            })
            .collect();

        let state = (*self.probe).chain_state_ref();
        Ok(&state.scan_chain)
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
    /// until the TAP passes through `Update-*`. Every batch that exchanges
    /// must end with this call.
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
    use std::fmt;

    use super::*;
    use crate::probe::{
        BatchExecutionError, CommandResult, DebugProbe, JtagChainAccess, JtagChainState, JtagOp,
        JtagProbe, JtagSequence, WireProtocol,
    };
    use bitvec::vec::BitVec;

    struct BatchRecorder {
        exchanges: Vec<BitSequence>,
        jtag_state: JtagChainState,
    }

    impl BatchRecorder {
        fn new() -> Self {
            Self {
                exchanges: Vec::new(),
                jtag_state: JtagChainState::default(),
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
                if let JtagOp::Exchange { data, capture } = op {
                    self.exchanges.push(data.clone());
                    if *capture && id.should_capture() {
                        let byte_len = data.len().div_ceil(8);
                        let mut bytes = vec![0u8; byte_len];
                        for (index, bit) in data.iter().enumerate() {
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

        fn shift_raw_sequence(
            &mut self,
            _sequence: JtagSequence,
        ) -> Result<BitVec, DebugProbeError> {
            Ok(BitVec::new())
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
    fn select_outside_chain_errors() {
        let mut probe = BatchRecorder::new();
        probe.jtag_state.scan_chain = three_tap_chain();
        let mut chain = JtagChain::new(&mut probe);
        assert!(matches!(
            chain.select(3),
            Err(DebugProbeError::TargetNotFound)
        ));
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
