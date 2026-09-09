//! Scan chain state and TAP batch composition.

use probe_rs_target::ScanChainElement;

use super::{JtagBatch, JtagProbe, TapState};
use crate::probe::common::{common_sequence, extract_idcodes, extract_ir_lengths};
use crate::probe::{BatchError, BitSequence, DebugProbeError, Handle, Results};

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

/// Scan chain driver built on [`JtagProbe`].
pub struct JtagChain<'p> {
    probe: &'p mut dyn JtagProbe,
    chain: Vec<ScanChainElement>,
    expected: Option<Vec<ScanChainElement>>,
    params: ChainParams,
}

impl<'p> JtagChain<'p> {
    /// Create a chain driver over `probe` with the given state.
    pub fn new(
        probe: &'p mut dyn JtagProbe,
        chain: Vec<ScanChainElement>,
        expected: Option<Vec<ScanChainElement>>,
        params: ChainParams,
    ) -> Self {
        Self {
            probe,
            chain,
            expected,
            params,
        }
    }

    /// Configure padding for the TAP at `tap`.
    pub fn select(&mut self, tap: usize) -> Result<(), DebugProbeError> {
        let Some(params) = ChainParams::from_jtag_chain(&self.chain, tap) else {
            return Err(DebugProbeError::TargetNotFound);
        };

        tracing::debug!("Selecting JTAG TAP: {tap}");
        tracing::debug!("Setting chain params: {params:?}");

        self.params = params;
        Ok(())
    }

    /// Set the expected scan chain used to validate IR lengths.
    pub fn set_expected(&mut self, chain: &[ScanChainElement]) {
        self.expected = Some(chain.to_vec());
    }

    /// Set the scan chain without measuring it.
    pub fn set_chain(&mut self, chain: &[ScanChainElement]) {
        self.chain = chain.to_vec();
    }

    /// Return the current scan chain.
    pub fn chain(&self) -> &[ScanChainElement] {
        &self.chain
    }

    /// Return the current chain padding parameters.
    pub fn params(&self) -> ChainParams {
        self.params
    }

    /// Take owned chain state back from the driver.
    pub fn into_parts(
        self,
    ) -> (
        Vec<ScanChainElement>,
        Option<Vec<ScanChainElement>>,
        ChainParams,
    ) {
        (self.chain, self.expected, self.params)
    }

    /// Measure the scan chain when it is not already set.
    pub fn scan_chain(&mut self) -> Result<&[ScanChainElement], DebugProbeError> {
        if !self.chain.is_empty() {
            return Ok(self.chain.as_slice());
        }

        const MAX_CHAIN: usize = 8;

        self.tap_reset_run()?;

        self.params = ChainParams::default();

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

        let ir_lens = extract_ir_lengths(
            response,
            idcodes.len(),
            self.expected
                .as_ref()
                .map(|chain| {
                    chain
                        .iter()
                        .filter_map(|s| s.ir_len)
                        .map(|s| s as usize)
                        .collect::<Vec<usize>>()
                })
                .as_deref(),
        )?;

        tracing::info!("Found {} TAPs on reset scan", idcodes.len());
        tracing::debug!("Detected IR lens: {:?}", ir_lens);

        self.chain = idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| ScanChainElement {
                ir_len: Some(irlen as u8),
                name: idcode.map(|i| i.to_string()),
            })
            .collect();

        Ok(self.chain.as_slice())
    }

    fn tap_reset_run(&mut self) -> Result<(), DebugProbeError> {
        let mut batch = JtagBatch::new();
        self.tap_reset(&mut batch);
        self.run(batch)?;
        Ok(())
    }

    /// Schedule an IR write with chain padding.
    pub fn shift_ir(&mut self, batch: &mut JtagBatch, ir: &BitSequence) {
        let mut data = BitSequence::repeat(true, self.params.irpre);
        data.extend(ir);
        data.extend(&BitSequence::repeat(true, self.params.irpost));
        batch.enter(TapState::ShiftIr);
        batch.exchange_no_capture(data);
    }

    /// Schedule a DR exchange with chain padding.
    pub fn exchange_dr(&mut self, batch: &mut JtagBatch, dr: &BitSequence) -> Handle<BitSequence> {
        let drpre = self.params.drpre;
        let dr_len = dr.len();
        let mut data = BitSequence::repeat(false, drpre);
        data.extend(dr);
        data.extend(&BitSequence::repeat(false, self.params.drpost));
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
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;
    use crate::probe::{BatchExecutionError, CommandResult, DebugProbe, JtagOp, WireProtocol};

    struct BatchRecorder {
        exchanges: Vec<BitSequence>,
    }

    impl BatchRecorder {
        fn new() -> Self {
            Self {
                exchanges: Vec::new(),
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
        let mut jtag_chain =
            JtagChain::new(&mut probe, chain.to_vec(), None, ChainParams::default());
        jtag_chain.select(tap).unwrap();
        jtag_chain.params()
    }

    fn run_shift_ir(probe: &mut BatchRecorder, params: ChainParams) -> BitSequence {
        let mut chain = JtagChain::new(probe, Vec::new(), None, params);
        let mut batch = JtagBatch::new();
        chain.shift_ir(&mut batch, &BitSequence::from_u64(4, 0b1010));
        chain.run(batch).unwrap();
        probe.exchanges.pop().unwrap()
    }

    fn run_exchange_dr(probe: &mut BatchRecorder, params: ChainParams) -> BitSequence {
        let mut chain = JtagChain::new(probe, Vec::new(), None, params);
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
        let mut chain = JtagChain::new(&mut probe, three_tap_chain(), None, ChainParams::default());
        assert!(matches!(
            chain.select(3),
            Err(DebugProbeError::TargetNotFound)
        ));
    }
}
