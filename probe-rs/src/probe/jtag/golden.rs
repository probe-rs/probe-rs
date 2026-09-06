use std::fmt;
use std::iter;

use bitvec::prelude::*;

use crate::probe::{ChainParams, DebugProbe, DebugProbeError, JtagChainState, WireProtocol};

use super::{BitSequence, BitbangJtag, JtagBatch, TapState, run_bitbang_batch};

#[derive(Clone, Copy, PartialEq, Debug)]
enum RegisterState {
    Select,
    Capture,
    Shift,
    Exit1,
    Pause,
    Exit2,
    Update,
}

impl RegisterState {
    fn step_toward(self, target: Self) -> bool {
        match self {
            Self::Select => false,
            Self::Capture if matches!(target, Self::Shift) => false,
            Self::Exit1 if matches!(target, Self::Pause | Self::Exit2) => false,
            Self::Exit2 if matches!(target, Self::Shift | Self::Exit1 | Self::Pause) => false,
            Self::Update => unreachable!(),
            _ => true,
        }
    }

    fn update(self, tms: bool) -> Self {
        if tms {
            match self {
                Self::Capture | Self::Shift => Self::Exit1,
                Self::Exit1 | Self::Exit2 => Self::Update,
                Self::Pause => Self::Exit2,
                Self::Select | Self::Update => unreachable!(),
            }
        } else {
            match self {
                Self::Select => Self::Capture,
                Self::Capture | Self::Shift => Self::Shift,
                Self::Exit1 | Self::Pause => Self::Pause,
                Self::Exit2 => Self::Shift,
                Self::Update => unreachable!(),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum OldJtagState {
    Reset,
    Idle,
    Dr(RegisterState),
    Ir(RegisterState),
}

impl OldJtagState {
    fn step_toward(self, target: Self) -> Option<bool> {
        let tms = match self {
            state if target == state => return None,
            Self::Reset => false,
            Self::Idle => true,
            Self::Dr(RegisterState::Select) => !matches!(target, Self::Dr(_)),
            Self::Ir(RegisterState::Select) => !matches!(target, Self::Ir(_)),
            Self::Dr(RegisterState::Update) | Self::Ir(RegisterState::Update) => {
                matches!(target, Self::Ir(_) | Self::Dr(_))
            }
            Self::Dr(state) => {
                let next = if let Self::Dr(target) = target {
                    target
                } else {
                    RegisterState::Update
                };
                state.step_toward(next)
            }
            Self::Ir(state) => {
                let next = if let Self::Ir(target) = target {
                    target
                } else {
                    RegisterState::Update
                };
                state.step_toward(next)
            }
        };
        Some(tms)
    }

    fn update(&mut self, tms: bool) {
        *self = match *self {
            Self::Reset if tms => Self::Reset,
            Self::Reset => Self::Idle,
            Self::Idle if tms => Self::Dr(RegisterState::Select),
            Self::Idle => Self::Idle,
            Self::Dr(RegisterState::Select) if tms => Self::Ir(RegisterState::Select),
            Self::Ir(RegisterState::Select) if tms => Self::Reset,
            Self::Dr(RegisterState::Update) | Self::Ir(RegisterState::Update) => {
                if tms {
                    Self::Dr(RegisterState::Select)
                } else {
                    Self::Idle
                }
            }
            Self::Dr(state) => Self::Dr(state.update(tms)),
            Self::Ir(state) => Self::Ir(state.update(tms)),
        };
    }
}

struct OldPathRecorder {
    state: OldJtagState,
    chain_params: ChainParams,
    triples: Vec<(bool, bool, bool)>,
    captured: BitVec,
}

impl OldPathRecorder {
    fn new() -> Self {
        Self {
            state: OldJtagState::Reset,
            chain_params: ChainParams::default(),
            triples: Vec::new(),
            captured: BitVec::new(),
        }
    }

    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.triples.push((tms, tdi, capture));
        if capture {
            self.captured.push(false);
        }
        self.state.update(tms);
        Ok(())
    }

    fn shift_bits(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: impl IntoIterator<Item = bool>,
    ) -> Result<(), DebugProbeError> {
        for ((tms, tdi), cap) in tms.into_iter().zip(tdi).zip(cap) {
            self.shift_bit(tms, tdi, cap)?;
        }
        Ok(())
    }

    fn take_triples(&mut self) -> Vec<(bool, bool, bool)> {
        std::mem::take(&mut self.triples)
    }
}

fn jtag_move_to_state(
    protocol: &mut OldPathRecorder,
    target: OldJtagState,
) -> Result<(), DebugProbeError> {
    while let Some(tms) = protocol.state.step_toward(target) {
        protocol.shift_bit(tms, false, false)?;
    }
    Ok(())
}

fn shift_ir(
    protocol: &mut OldPathRecorder,
    data: &[u8],
    len: usize,
    capture_data: bool,
) -> Result<(), DebugProbeError> {
    let pre_bits = protocol.chain_params.irpre;
    let post_bits = protocol.chain_params.irpost;
    let tms_data = std::iter::repeat_n(false, len - 1);

    jtag_move_to_state(protocol, OldJtagState::Ir(RegisterState::Shift))?;

    let tms = std::iter::repeat_n(false, pre_bits)
        .chain(tms_data)
        .chain(std::iter::repeat_n(false, post_bits))
        .chain(iter::once(true));

    let tdi = std::iter::repeat_n(true, pre_bits)
        .chain(data.as_bits::<Lsb0>()[..len].iter().map(|b| *b))
        .chain(std::iter::repeat_n(true, post_bits));

    let capture = std::iter::repeat_n(false, pre_bits)
        .chain(std::iter::repeat_n(capture_data, len))
        .chain(iter::repeat(false));

    protocol.shift_bits(tms, tdi, capture)?;
    jtag_move_to_state(protocol, OldJtagState::Ir(RegisterState::Update))?;

    Ok(())
}

fn shift_dr(
    protocol: &mut OldPathRecorder,
    data: &[u8],
    register_bits: usize,
    capture_data: bool,
    idle_cycles: usize,
) -> Result<usize, DebugProbeError> {
    let tms_shift_out_value = std::iter::repeat_n(false, register_bits - 1);

    jtag_move_to_state(protocol, OldJtagState::Dr(RegisterState::Shift))?;

    let pre_bits = protocol.chain_params.drpre;
    let post_bits = protocol.chain_params.drpost;

    let tms = std::iter::repeat_n(false, pre_bits)
        .chain(tms_shift_out_value)
        .chain(std::iter::repeat_n(false, post_bits))
        .chain(iter::once(true));

    let tdi = std::iter::repeat_n(false, pre_bits)
        .chain(data.as_bits::<Lsb0>()[..register_bits].iter().map(|b| *b))
        .chain(std::iter::repeat_n(false, post_bits));

    let capture = std::iter::repeat_n(false, pre_bits)
        .chain(std::iter::repeat_n(capture_data, register_bits))
        .chain(iter::repeat(false));

    protocol.shift_bits(tms, tdi, capture)?;

    jtag_move_to_state(protocol, OldJtagState::Dr(RegisterState::Update))?;

    if idle_cycles > 0 {
        jtag_move_to_state(protocol, OldJtagState::Idle)?;
        let tms = std::iter::repeat_n(false, idle_cycles);
        let tdi = std::iter::repeat_n(false, idle_cycles);
        protocol.shift_bits(tms, tdi, iter::repeat(false))?;
    }

    if capture_data {
        Ok(register_bits)
    } else {
        Ok(0)
    }
}

struct GoldenRecorder {
    jtag_state: JtagChainState,
    triples: Vec<(bool, bool, bool)>,
    captured: BitVec,
}

impl GoldenRecorder {
    fn new() -> Self {
        Self {
            jtag_state: JtagChainState::default(),
            triples: Vec::new(),
            captured: BitVec::new(),
        }
    }

    fn take_triples(&mut self) -> Vec<(bool, bool, bool)> {
        std::mem::take(&mut self.triples)
    }
}

impl fmt::Debug for GoldenRecorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoldenRecorder")
            .field("triples", &self.triples.len())
            .finish_non_exhaustive()
    }
}

impl DebugProbe for GoldenRecorder {
    fn get_name(&self) -> &str {
        "golden recorder"
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

impl BitbangJtag for GoldenRecorder {
    fn tap_state(&mut self) -> &mut TapState {
        &mut self.jtag_state.tap_state
    }

    fn shift(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.triples.push((tms, tdi, capture));
        if capture {
            self.captured.push(false);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn captured(&mut self) -> Result<BitVec, DebugProbeError> {
        Ok(std::mem::take(&mut self.captured))
    }
}

fn tap_to_jtag(tap: TapState) -> OldJtagState {
    match tap {
        TapState::TestLogicReset => OldJtagState::Reset,
        TapState::RunTestIdle => OldJtagState::Idle,
        TapState::ShiftIr => OldJtagState::Ir(RegisterState::Shift),
        TapState::ShiftDr => OldJtagState::Dr(RegisterState::Shift),
        TapState::PauseIr => OldJtagState::Ir(RegisterState::Pause),
        TapState::PauseDr => OldJtagState::Dr(RegisterState::Pause),
    }
}

fn triples_to_strings(triples: &[(bool, bool, bool)]) -> (String, String, String) {
    let mut tms = String::new();
    let mut tdi = String::new();
    let mut cap = String::new();
    for &(t, d, c) in triples {
        tms.push(if t { '1' } else { '0' });
        tdi.push(if d { '1' } else { '0' });
        cap.push(if c { '1' } else { '0' });
    }
    (tms, tdi, cap)
}

pub(crate) fn assert_triples_eq(actual: &[(bool, bool, bool)], tms: &str, tdi: &str, cap: &str) {
    let (at, ad, ac) = triples_to_strings(actual);
    assert_eq!(at, tms, "TMS mismatch");
    assert_eq!(ad, tdi, "TDI mismatch");
    assert_eq!(ac, cap, "capture mismatch");
}

const IR_VALUE: u8 = 0b10110;
const IR_LEN: usize = 5;

pub(crate) fn one_tap_params() -> ChainParams {
    ChainParams {
        irlen: IR_LEN,
        ..ChainParams::default()
    }
}

pub(crate) fn three_tap_params() -> ChainParams {
    ChainParams {
        index: 1,
        irpre: 4,
        irpost: 5,
        drpre: 1,
        drpost: 1,
        irlen: IR_LEN,
    }
}

pub(crate) fn build_ir_exchange(params: ChainParams, value: u8, len: usize) -> BitSequence {
    let mut data = BitSequence::new();
    for _ in 0..params.irpre {
        data.push(true);
    }
    for i in 0..len {
        data.push(value & (1 << i) != 0);
    }
    for _ in 0..params.irpost {
        data.push(true);
    }
    data
}

pub(crate) fn build_dr_exchange(params: ChainParams, bytes: &[u8], len: usize) -> BitSequence {
    let mut data = BitSequence::new();
    for _ in 0..params.drpre {
        data.push(false);
    }
    for i in 0..len {
        data.push(bytes.as_bits::<Lsb0>()[i]);
    }
    for _ in 0..params.drpost {
        data.push(false);
    }
    data
}

fn record_move_to_state(from: TapState, to: TapState) -> Vec<(bool, bool, bool)> {
    let mut recorder = OldPathRecorder::new();
    recorder.state = tap_to_jtag(from);
    jtag_move_to_state(&mut recorder, tap_to_jtag(to)).unwrap();
    recorder.take_triples()
}

fn record_shift_ir(params: ChainParams) -> Vec<(bool, bool, bool)> {
    let mut recorder = OldPathRecorder::new();
    recorder.chain_params = params;
    shift_ir(&mut recorder, &[IR_VALUE], IR_LEN, false).unwrap();
    jtag_move_to_state(&mut recorder, OldJtagState::Idle).unwrap();
    recorder.take_triples()
}

fn record_shift_dr(params: ChainParams, bytes: &[u8], len: usize) -> Vec<(bool, bool, bool)> {
    let mut recorder = OldPathRecorder::new();
    recorder.chain_params = params;
    shift_dr(&mut recorder, bytes, len, false, 0).unwrap();
    jtag_move_to_state(&mut recorder, OldJtagState::Idle).unwrap();
    recorder.take_triples()
}

fn record_register_write(
    params: ChainParams,
    bytes: &[u8],
    len: usize,
    idle_cycles: u32,
) -> Vec<(bool, bool, bool)> {
    let mut recorder = OldPathRecorder::new();
    recorder.chain_params = params;
    shift_ir(&mut recorder, &[IR_VALUE], IR_LEN, false).unwrap();
    shift_dr(&mut recorder, bytes, len, false, 0).unwrap();
    jtag_move_to_state(&mut recorder, OldJtagState::Idle).unwrap();
    if idle_cycles > 0 {
        let tms = std::iter::repeat_n(false, idle_cycles as usize);
        let tdi = std::iter::repeat_n(false, idle_cycles as usize);
        recorder.shift_bits(tms, tdi, iter::repeat(false)).unwrap();
    }
    recorder.take_triples()
}

fn record_reset() -> Vec<(bool, bool, bool)> {
    let mut recorder = OldPathRecorder::new();
    let tms = [true, true, true, true, true, false];
    let tdi = std::iter::repeat(true);
    recorder
        .shift_bits(tms, tdi, std::iter::repeat(false))
        .unwrap();
    recorder.take_triples()
}

pub(crate) fn lowering_move_to(from: TapState, to: TapState) -> Vec<(bool, bool, bool)> {
    let mut recorder = GoldenRecorder::new();
    let mut batch = JtagBatch::new();
    batch.enter(to);
    run_bitbang_batch(&mut recorder, from, &batch).unwrap();
    recorder.take_triples()
}

pub(crate) fn lowering_shift_ir(params: ChainParams) -> Vec<(bool, bool, bool)> {
    let mut recorder = GoldenRecorder::new();
    let mut batch = JtagBatch::new();
    batch.enter(TapState::ShiftIr);
    batch.exchange_no_capture(build_ir_exchange(params, IR_VALUE, IR_LEN));
    batch.enter(TapState::RunTestIdle);
    run_bitbang_batch(&mut recorder, TapState::TestLogicReset, &batch).unwrap();
    recorder.take_triples()
}

pub(crate) fn lowering_shift_dr(
    params: ChainParams,
    bytes: &[u8],
    len: usize,
) -> Vec<(bool, bool, bool)> {
    let mut recorder = GoldenRecorder::new();
    let mut batch = JtagBatch::new();
    batch.enter(TapState::ShiftDr);
    batch.exchange_no_capture(build_dr_exchange(params, bytes, len));
    batch.enter(TapState::RunTestIdle);
    run_bitbang_batch(&mut recorder, TapState::TestLogicReset, &batch).unwrap();
    recorder.take_triples()
}

pub(crate) fn lowering_register_write(
    params: ChainParams,
    bytes: &[u8],
    len: usize,
    idle_cycles: u32,
) -> Vec<(bool, bool, bool)> {
    let mut recorder = GoldenRecorder::new();
    recorder.jtag_state.chain_params = params;
    let mut batch = JtagBatch::new();
    batch.enter(TapState::ShiftIr);
    batch.exchange_no_capture(build_ir_exchange(params, IR_VALUE, IR_LEN));
    batch.enter(TapState::ShiftDr);
    batch.exchange_no_capture(build_dr_exchange(params, bytes, len));
    batch.enter(TapState::RunTestIdle);
    if idle_cycles > 0 {
        batch.clock(idle_cycles);
    }
    run_bitbang_batch(&mut recorder, TapState::TestLogicReset, &batch).unwrap();
    recorder.take_triples()
}

fn lowering_reset_tlr_only() -> Vec<(bool, bool, bool)> {
    let mut recorder = GoldenRecorder::new();
    let mut batch = JtagBatch::new();
    batch.enter(TapState::TestLogicReset);
    run_bitbang_batch(&mut recorder, TapState::TestLogicReset, &batch).unwrap();
    recorder.take_triples()
}

// Placeholder - will be filled after running print_golden
include!("golden_literals.rs");

#[cfg(test)]
mod tests {
    use super::*;

    const STABLE_STATES: [TapState; 6] = [
        TapState::TestLogicReset,
        TapState::RunTestIdle,
        TapState::ShiftIr,
        TapState::ShiftDr,
        TapState::PauseIr,
        TapState::PauseDr,
    ];

    #[test]
    fn recorder_move_to_state_matches_golden() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                let triples = record_move_to_state(from, to);
                let (tms, tdi, cap) = move_literal(from, to);
                assert_triples_eq(&triples, tms, tdi, cap);
            }
        }
    }

    #[test]
    fn lowering_move_to_state_matches_golden_except_tlr_target() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                if to == TapState::TestLogicReset {
                    continue;
                }
                let triples = lowering_move_to(from, to);
                let (tms, tdi, cap) = move_literal(from, to);
                assert_triples_eq(&triples, tms, tdi, cap);
            }
        }
    }

    #[test]
    fn lowering_move_to_tlr_uses_path_to() {
        let triples = lowering_move_to(TapState::RunTestIdle, TapState::TestLogicReset);
        assert_eq!(triples_to_strings(&triples).0, "11111");
    }

    #[test]
    fn recorder_shift_ir_matches_golden() {
        let one = record_shift_ir(one_tap_params());
        assert_triples_eq(
            &one,
            SHIFT_IR_ONE_TAP.0,
            SHIFT_IR_ONE_TAP.1,
            SHIFT_IR_ONE_TAP.2,
        );

        let three = record_shift_ir(three_tap_params());
        assert_triples_eq(
            &three,
            SHIFT_IR_THREE_TAP.0,
            SHIFT_IR_THREE_TAP.1,
            SHIFT_IR_THREE_TAP.2,
        );
    }

    #[test]
    fn lowering_shift_ir_matches_golden() {
        let one = lowering_shift_ir(one_tap_params());
        assert_triples_eq(
            &one,
            SHIFT_IR_ONE_TAP.0,
            SHIFT_IR_ONE_TAP.1,
            SHIFT_IR_ONE_TAP.2,
        );

        let three = lowering_shift_ir(three_tap_params());
        assert_triples_eq(
            &three,
            SHIFT_IR_THREE_TAP.0,
            SHIFT_IR_THREE_TAP.1,
            SHIFT_IR_THREE_TAP.2,
        );
    }

    #[test]
    fn recorder_shift_dr_matches_golden() {
        let cases = [
            (SHIFT_DR_ONE_TAP_ONE, one_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_ONE_TAP_THIRTY_TWO,
                one_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_ONE_TAP_FORTY_ONE,
                one_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_ONE_TAP_SIXTY_FOUR,
                one_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
            (SHIFT_DR_THREE_TAP_ONE, three_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_THREE_TAP_THIRTY_TWO,
                three_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_THREE_TAP_FORTY_ONE,
                three_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_THREE_TAP_SIXTY_FOUR,
                three_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
        ];
        for (literal, params, bytes, len) in cases {
            let triples = record_shift_dr(params, bytes, len);
            assert_triples_eq(&triples, literal.0, literal.1, literal.2);
        }
    }

    #[test]
    fn lowering_shift_dr_matches_golden() {
        let cases = [
            (SHIFT_DR_ONE_TAP_ONE, one_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_ONE_TAP_THIRTY_TWO,
                one_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_ONE_TAP_FORTY_ONE,
                one_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_ONE_TAP_SIXTY_FOUR,
                one_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
            (SHIFT_DR_THREE_TAP_ONE, three_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_THREE_TAP_THIRTY_TWO,
                three_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_THREE_TAP_FORTY_ONE,
                three_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_THREE_TAP_SIXTY_FOUR,
                three_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
        ];
        for (literal, params, bytes, len) in cases {
            let triples = lowering_shift_dr(params, bytes, len);
            assert_triples_eq(&triples, literal.0, literal.1, literal.2);
        }
    }

    #[test]
    fn recorder_reset_matches_golden() {
        let triples = record_reset();
        assert_triples_eq(&triples, RESET.0, RESET.1, RESET.2);
    }

    #[test]
    fn lowering_reset_tlr_bits_match_golden() {
        let triples = lowering_reset_tlr_only();
        assert_triples_eq(
            &triples,
            RESET_TLR_BITS.0,
            RESET_TLR_BITS.1,
            RESET_TLR_BITS.2,
        );
    }

    #[test]
    fn lowering_register_write_with_eight_idle_cycles_matches_golden() {
        let bytes = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
        let params = one_tap_params();
        let triples = lowering_register_write(params, &bytes, 41, 8);
        assert_triples_eq(
            &triples,
            REGISTER_WRITE_EIGHT_IDLE.0,
            REGISTER_WRITE_EIGHT_IDLE.1,
            REGISTER_WRITE_EIGHT_IDLE.2,
        );
    }

    #[test]
    fn recorder_register_write_with_eight_idle_cycles_matches_golden() {
        let bytes = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
        let params = one_tap_params();
        let triples = record_register_write(params, &bytes, 41, 8);
        assert_triples_eq(
            &triples,
            REGISTER_WRITE_EIGHT_IDLE.0,
            REGISTER_WRITE_EIGHT_IDLE.1,
            REGISTER_WRITE_EIGHT_IDLE.2,
        );
    }
}
