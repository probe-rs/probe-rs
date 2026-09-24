//! JTAG on the chip's byte shifts and bit ops.
//!
//! Based on the JTAG driver in openOCD & openFPGAloader.

use bitvec::{field::BitField, slice::BitSlice, vec::BitVec};

use crate::probe::{
    BatchExecutionError, BitSequence, DebugProbeError, JtagBatch, JtagOp, Results,
    jtag::{TapState, distribute_captures, enter_tdi, exchange_leaves_shift},
};

use super::capabilities::Pack;
use super::device::Ch347Device;
use super::transport::{HEADER_LEN, frame};

const CMD_JTAG_INIT: u8 = 0xD0;
const CMD_BIT_OP: u8 = 0xD1;
const CMD_BIT_OP_RD: u8 = 0xD2;
const CMD_SHIFT: u8 = 0xD3;
const CMD_SHIFT_RD: u8 = 0xD4;

/// No firmware accepts this clock index; the reply byte tells the pack mode apart instead.
const PACK_PROBE_INDEX: u8 = 9;

/// Payload bytes one command carries at most.
const MAX_PAYLOAD: usize = 507;
/// TCK cycles one bit-op command carries at most.
const MAX_BIT_OP_CYCLES: usize = 248;
/// TDO bits the chip buffers per round, one per bit-op cycle and eight per shifted byte.
const MAX_ROUND_TDO: usize = 4096;
/// Bytes a round stays below, one USB packet or the buffer of a chip that answers the pack
/// probe.
const STANDARD_ROUND: usize = 512;
const LARGER_ROUND: usize = 51200;

const PIN_TCK: u8 = 1 << 0;
const PIN_TMS: u8 = 1 << 1;
const PIN_TDI: u8 = 1 << 4;
/// TRST is push-pull and shares a pin with GPIO3; it stays high so JTAG traffic never resets a target.
const PIN_TRST: u8 = 1 << 5;

/// One TCK cycle of a bit op.
#[derive(Debug, Clone, Copy)]
struct Cycle {
    tms: bool,
    tdi: bool,
}

impl Cycle {
    /// The pin byte with TCK low.
    fn pins(self) -> u8 {
        PIN_TRST | (u8::from(self.tms) * PIN_TMS) | (u8::from(self.tdi) * PIN_TDI)
    }
}

/// What becomes of the TDO bits of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tdo {
    /// Not read, `0xD1` or `0xD3`.
    Ignore,
    /// Read and dropped, so that a round ends on a reply.
    Discard,
    /// Read for an exchange.
    Capture,
}

/// One of the D1-D4 commands.
#[derive(Debug)]
enum Command {
    /// Bit ops. `0xD2` reads a TDO byte per rising edge.
    BitOps { cycles: Vec<Cycle>, tdo: Tdo },
    /// A byte shift of TDI bytes, LSB first. `0xD4` reads a TDO byte per TDI byte.
    Shift { tdi: Vec<u8>, tdo: Tdo },
}

impl Command {
    fn tdo(&self) -> Tdo {
        match self {
            Command::BitOps { tdo, .. } | Command::Shift { tdo, .. } => *tdo,
        }
    }

    fn read(&self) -> bool {
        self.tdo() != Tdo::Ignore
    }

    fn code(&self) -> u8 {
        match (self, self.read()) {
            (Command::BitOps { .. }, false) => CMD_BIT_OP,
            (Command::BitOps { .. }, true) => CMD_BIT_OP_RD,
            (Command::Shift { .. }, false) => CMD_SHIFT,
            (Command::Shift { .. }, true) => CMD_SHIFT_RD,
        }
    }

    fn payload_len(&self) -> usize {
        match self {
            Command::BitOps { cycles, .. } => 2 * cycles.len() + 1,
            Command::Shift { tdi, .. } => tdi.len(),
        }
    }

    /// The reply's payload length if it reads.
    fn reply_len(&self) -> usize {
        match self {
            Command::BitOps { cycles, .. } => cycles.len(),
            Command::Shift { tdi, .. } => tdi.len(),
        }
    }

    fn frame(&self) -> Vec<u8> {
        match self {
            Command::BitOps { cycles, .. } => {
                let pins = cycles
                    .iter()
                    .flat_map(|cycle| [cycle.pins(), cycle.pins() | PIN_TCK]);
                let last = cycles.last().expect("a bit op has cycles").pins();
                frame(self.code(), &pins.chain([last]).collect::<Vec<_>>())
            }
            Command::Shift { tdi, .. } => frame(self.code(), tdi),
        }
    }
}

/// What the chip allows.
#[derive(Debug, Clone, Copy)]
struct Wire {
    /// Whether whole bytes go as byte shifts.
    bytewise: bool,
    /// Bytes a round stays below.
    round_bytes: usize,
}

impl Wire {
    fn new(pack: Pack, bytewise: bool) -> Self {
        let round_bytes = match pack {
            Pack::Larger if bytewise => LARGER_ROUND,
            _ => STANDARD_ROUND,
        };
        Self {
            bytewise,
            round_bytes,
        }
    }
}

/// What a round carries against the chip's buffers.
#[derive(Debug, Clone, Copy, Default)]
struct Load {
    bytes: usize,
    tdo: usize,
}

impl Load {
    /// The load with `command` added, counting its TDO if it reads or if `read`.
    fn plus(self, command: &Command, read: bool) -> Self {
        let clocks = match command {
            Command::BitOps { cycles, .. } => cycles.len(),
            Command::Shift { tdi, .. } => 8 * tdi.len(),
        };
        Self {
            bytes: self.bytes + HEADER_LEN + command.payload_len(),
            tdo: self.tdo + if read || command.read() { clocks } else { 0 },
        }
    }

    fn fits(self, wire: Wire) -> bool {
        self.bytes < wire.round_bytes && self.tdo < MAX_ROUND_TDO
    }
}

/// The commands of a batch, merging adjacent commands of one kind up to their limits.
#[derive(Debug)]
struct Encoder {
    wire: Wire,
    commands: Vec<Command>,
    /// Where the batch leaves the TAP.
    state: TapState,
    /// Whether the last pin byte had TMS low, which a byte shift needs, since it may hold
    /// TMS where it was.
    tms_low: bool,
}

impl Encoder {
    fn cycle(&mut self, cycle: Cycle, tdo: Tdo) {
        self.tms_low = !cycle.tms;
        match self.commands.last_mut() {
            Some(Command::BitOps { cycles, tdo: last })
                if *last == tdo && cycles.len() < MAX_BIT_OP_CYCLES =>
            {
                cycles.push(cycle)
            }
            _ => self.commands.push(Command::BitOps {
                cycles: vec![cycle],
                tdo,
            }),
        }
    }

    /// Shifts `bits` with TMS low, whole bytes as byte shifts once TMS is known low. With
    /// `leave`, the last bit raises TMS as the first step out of Shift.
    fn shift(&mut self, bits: &BitSlice<u8>, tdo: Tdo, leave: bool) {
        let (bits, leaving) = bits.split_at(bits.len() - usize::from(leave));
        for chunk in bits.chunks(8) {
            if chunk.len() < 8 || !(self.wire.bytewise && self.tms_low) {
                for tdi in chunk.iter().by_vals() {
                    self.cycle(Cycle { tms: false, tdi }, tdo);
                }
                continue;
            }
            let byte = chunk.load_le();
            match self.commands.last_mut() {
                Some(Command::Shift { tdi, tdo: last })
                    if *last == tdo && tdi.len() < MAX_PAYLOAD =>
                {
                    tdi.push(byte)
                }
                _ => self.commands.push(Command::Shift {
                    tdi: vec![byte],
                    tdo,
                }),
            }
        }
        for tdi in leaving.iter().by_vals() {
            self.cycle(Cycle { tms: true, tdi }, tdo);
        }
    }
}

/// Encodes a batch that starts with the TAP in `start` and TMS low if `tms_low`.
fn encode(
    start: TapState,
    tms_low: bool,
    wire: Wire,
    batch: &JtagBatch,
) -> Result<Encoder, DebugProbeError> {
    let ops: Vec<_> = batch.iter().collect();
    let mut encoder = Encoder {
        wire,
        commands: Vec::new(),
        state: start,
        tms_low,
    };
    // The first step of a path that the last exchange bit already took.
    let mut skip = 0;
    for (index, (id, op)) in ops.iter().enumerate() {
        let state = encoder.state;
        match op {
            JtagOp::EnterState(target) => {
                let tdi = enter_tdi(*target);
                for &tms in &state.path_to(*target)[skip..] {
                    encoder.cycle(Cycle { tms, tdi }, Tdo::Ignore);
                }
                skip = 0;
                encoder.state = *target;
            }
            JtagOp::Exchange { data, capture } => {
                if !matches!(state, TapState::ShiftIr | TapState::ShiftDr) {
                    return Err(DebugProbeError::Other(format!(
                        "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                    )));
                }
                let leave = !data.is_empty()
                    && exchange_leaves_shift(state, ops.get(index + 1).map(|(_, op)| op));
                let tdo = if *capture && id.should_capture() {
                    Tdo::Capture
                } else {
                    Tdo::Ignore
                };
                encoder.shift(data.as_bits(), tdo, leave);
                skip = usize::from(leave);
            }
            // Only bit ops can hold TMS high to stay in Test-Logic-Reset.
            JtagOp::ClockTck { count } if state == TapState::TestLogicReset => {
                for tms in std::iter::repeat_n(true, *count as usize) {
                    encoder.cycle(Cycle { tms, tdi: false }, Tdo::Ignore);
                }
            }
            JtagOp::ClockTck { count } => {
                let idle = BitSequence::repeat(false, *count as usize);
                encoder.shift(idle.as_bits(), Tdo::Ignore, false);
            }
        }
    }
    Ok(encoder)
}

/// Packs commands into rounds in order, each within the chip's buffers and ending on a
/// reading command.
///
/// Waiting for the reply of every round costs one USB round trip per round and shows a round
/// the chip dropped or cut short before the next one goes out.
fn rounds(commands: Vec<Command>, wire: Wire) -> Vec<Vec<Command>> {
    let mut rounds = Vec::new();
    let mut round = Vec::new();
    let mut load = Load::default();
    for command in commands {
        if !round.is_empty() && !load.plus(&command, true).fits(wire) {
            rounds.push(std::mem::take(&mut round));
            load = Load::default();
        }
        load = load.plus(&command, false);
        round.push(command);
    }
    if !round.is_empty() {
        rounds.push(round);
    }
    for round in &mut rounds {
        if let Some(Command::BitOps { tdo, .. } | Command::Shift { tdo, .. }) = round.last_mut()
            && *tdo == Tdo::Ignore
        {
            *tdo = Tdo::Discard;
        }
    }
    rounds
}

impl Ch347Device {
    pub(super) fn jtag_init(&mut self, index: u8) -> Result<u8, DebugProbeError> {
        self.jtag_tms_low = false;
        // The init drives TMS and TRST high.
        let pins = PIN_TMS | PIN_TRST;
        self.command_status(CMD_JTAG_INIT, &[0, index, pins, pins, pins, pins])
    }

    /// Runs the pack-mode probe once and remembers the answer.
    ///
    /// The JTAG init drives TRST high; SWD mode never needs it.
    pub(super) fn pack(&mut self) -> Result<Pack, DebugProbeError> {
        if let Some(pack) = self.pack {
            return Ok(pack);
        }
        let pack = match self.jtag_init(PACK_PROBE_INDEX)? {
            0 => Pack::Standard,
            _ => Pack::Larger,
        };
        tracing::debug!("{pack:?} pack mode");
        self.pack = Some(pack);
        Ok(pack)
    }

    /// Runs a batch that starts with the TAP in `tap`, and moves `tap` to where the batch
    /// leaves it once the wire has run.
    pub(crate) fn run_jtag_batch(
        &mut self,
        tap: &mut TapState,
        batch: &JtagBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let failed = |error| BatchExecutionError::new_from_debug_probe(error, Results::new());
        let pack = self.pack().map_err(failed)?;
        let wire = Wire::new(pack, self.capabilities.bytewise_jtag());
        let encoded = encode(*tap, self.jtag_tms_low, wire, batch).map_err(failed)?;
        let rounds = rounds(encoded.commands, wire);
        // Unknown until the last round has answered.
        self.jtag_tms_low = false;
        let mut captured = BitVec::new();
        for round in &rounds {
            let frames: Vec<_> = round.iter().flat_map(Command::frame).collect();
            let reading: Vec<_> = round.iter().filter(|command| command.read()).collect();
            let replies: Vec<_> = reading
                .iter()
                .map(|command| (command.code(), command.reply_len()))
                .collect();
            let payloads = self.round(&frames, &replies).map_err(failed)?;
            let mut at = 0;
            for command in reading {
                let reply = &payloads[at..at + command.reply_len()];
                at += reply.len();
                match command {
                    _ if command.tdo() != Tdo::Capture => {}
                    Command::BitOps { .. } => captured.extend(reply.iter().map(|b| b & 1 != 0)),
                    Command::Shift { .. } => {
                        captured.extend_from_bitslice(BitSlice::<u8>::from_slice(reply))
                    }
                }
            }
        }
        self.jtag_tms_low = encoded.tms_low;
        *tap = encoded.state;
        // Once per batch.
        if !rounds.is_empty() {
            self.led_activity(CMD_SHIFT);
        }
        distribute_captures(batch.iter(), &captured, Results::new())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    use super::super::device::tests::{CH347F_1_20, PACK_PROBE, device, on_transport};
    use super::super::transport::Transport;
    use super::{CMD_BIT_OP as D1, CMD_BIT_OP_RD as D2, CMD_SHIFT as D3, CMD_SHIFT_RD as D4, *};
    use crate::probe::jtag::golden::{
        build_dr_exchange, build_ir_exchange, lowering_batch, one_tap_params, three_tap_params,
    };

    fn wire(bytewise: bool) -> Wire {
        Wire::new(Pack::Larger, bytewise)
    }

    /// The code and payload length of every command, per round.
    fn sent(tms_low: bool, wire: Wire, batch: &JtagBatch) -> Vec<Vec<(u8, usize)>> {
        let encoded = encode(TapState::RunTestIdle, tms_low, wire, batch).unwrap();
        let shape = |command: &Command| (command.code(), command.payload_len());
        let shapes = |round: &Vec<Command>| round.iter().map(shape).collect();
        rounds(encoded.commands, wire).iter().map(shapes).collect()
    }

    /// (TMS, TDI, capture) per clock, a byte shift holding TMS where the last bit op left it.
    fn decode(commands: &[Command]) -> Vec<(bool, bool, bool)> {
        let (mut tms, mut triples) = (true, Vec::new());
        for command in commands {
            let capture = command.tdo() == Tdo::Capture;
            let cycles = match command {
                Command::BitOps { cycles, .. } => cycles.clone(),
                Command::Shift { tdi, .. } => {
                    let bits = BitVec::<u8>::from_slice(tdi);
                    bits.into_iter().map(|tdi| Cycle { tms, tdi }).collect()
                }
            };
            tms = cycles.last().unwrap().tms;
            triples.extend(cycles.iter().map(|cycle| (cycle.tms, cycle.tdi, capture)));
        }
        triples
    }

    #[test]
    fn golden_scans_match_the_bitbang_lowering_in_both_settings() {
        use TapState::{PauseDr, PauseIr, RunTestIdle, ShiftDr, ShiftIr, TestLogicReset};
        let states = [
            TestLogicReset,
            RunTestIdle,
            ShiftIr,
            ShiftDr,
            PauseIr,
            PauseDr,
        ];
        let bytes = [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11];
        let (mut batch, mut handles) = (JtagBatch::new(), Vec::new());
        for index in 0..36 {
            batch.enter(states[index / 6]);
            batch.enter(states[index % 6]);
        }
        for params in [one_tap_params(), three_tap_params()] {
            for len in [1, 32, 41, 64] {
                batch.enter(ShiftIr);
                batch.exchange_no_capture(build_ir_exchange(params, 0b10110, 5));
                batch.enter(ShiftDr);
                handles.push(batch.exchange(build_dr_exchange(params, &bytes, len)));
                batch.enter(RunTestIdle);
                batch.clock(8);
            }
        }
        let golden = lowering_batch(TestLogicReset, &batch);
        for bytewise in [true, false] {
            let encoded = encode(TestLogicReset, false, wire(bytewise), &batch).unwrap();
            assert_eq!(decode(&encoded.commands), golden, "bytewise {bytewise}");
        }
    }

    #[test]
    fn a_captured_49_bit_scan_is_six_byte_shifts_and_one_bit() {
        let (value, captured) = (0x1_2345_6789_ABCD, 0x1_4111_1043_8765);
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        let handle = batch.exchange(BitSequence::from_u64(49, value));
        batch.enter(TapState::RunTestIdle);
        // TMS high, low, low into Shift-DR, the last bit with TDI and TMS high, then TMS high
        // and low to Run-Test/Idle.
        let mut request = frame(D1, &[0x22, 0x23, 0x20, 0x21, 0x20, 0x21, 0x20]);
        request.extend(frame(D4, &u64::to_le_bytes(value)[..6]));
        request.extend(frame(D2, &[0x32, 0x33, 0x32]));
        request.extend(frame(D2, &[0x22, 0x23, 0x20, 0x21, 0x20]));
        let mut reply = frame(D4, &u64::to_le_bytes(captured)[..6]);
        reply.extend(frame(D2, &[1]));
        reply.extend(frame(D2, &[0, 0]));
        let (mut dev, script) = device(&[(&request, &reply)]);
        dev.pack = Some(Pack::Larger);
        let mut results = dev
            .run_jtag_batch(&mut TapState::RunTestIdle, &batch)
            .unwrap();
        assert!(script.finished() && dev.jtag_tms_low);
        let bits = results.take(handle).unwrap();
        assert_eq!(bits, BitSequence::from_u64(49, captured));
    }

    #[test]
    fn scans_split_into_byte_shifts_bit_ops_and_rounds() {
        let (path, bit, back) = ((D1, 7), (D2, 3), (D2, 5));
        let cases = [
            (8, false, vec![vec![path, (D4, 1)]]),
            (9, true, vec![vec![path, (D4, 1), bit, back]]),
            (49, true, vec![vec![path, (D4, 6), bit, back]]),
            (4057, true, vec![vec![path, (D4, 507), bit, back]]),
            // 5000 TDO bits split the round.
            (
                5000,
                true,
                vec![vec![path, (D4, 507)], vec![(D4, 117), (D2, 17), back]],
            ),
        ];
        for (len, leave, expected) in cases {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftDr);
            let _handle = batch.exchange(BitSequence::repeat(true, len));
            if leave {
                batch.enter(TapState::RunTestIdle);
            }
            assert_eq!(sent(false, wire(true), &batch), expected, "{len} bits");
        }
    }

    #[test]
    fn byte_shifts_wait_for_a_tms_low_pin_byte() {
        // After the init TMS is high, so the first idle byte goes as bit ops, which lower it.
        let mut batch = JtagBatch::new();
        batch.clock(24);
        batch.enter(TapState::ShiftDr);
        let wire = wire(true);
        assert_eq!(sent(false, wire, &batch), [[(D1, 17), (D3, 2), (D2, 7)]]);
        assert_eq!(sent(true, wire, &batch), [[(D3, 3), (D2, 7)]]);
    }

    #[test]
    fn the_pack_probe_picks_the_clock_table_and_the_init_may_refuse_it() {
        // 15 MHz is index 5 of the larger table and index 3 of the standard one.
        for (larger, index, status) in [(1, 5, 0), (0, 3, 0), (1, 5, 1)] {
            let init = [0xD0, 6, 0, 0, index, 0x22, 0x22, 0x22, 0x22];
            let replies = ([0xD0, 1, 0, larger], [0xD0, 1, 0, status]);
            let (mut dev, script) = device(&[(PACK_PROBE, &replies.0), (&init, &replies.1)]);
            // The init also forgets the TMS level.
            dev.jtag_tms_low = true;
            let refused = matches!(dev.attach(), Err(DebugProbeError::UnsupportedSpeed(15000)));
            assert_eq!((refused, dev.jtag_tms_low), (status == 1, false));
            assert!(script.finished());
        }
    }

    /// Answers every read with the next transfer, an empty one a zero-length packet.
    #[derive(Debug)]
    struct Transfers(VecDeque<Vec<u8>>);

    impl Transport for Transfers {
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8], _: Duration) -> io::Result<usize> {
            let transfer = self.0.pop_front().expect("a read without a reply");
            buf[..transfer.len()].copy_from_slice(&transfer);
            Ok(transfer.len())
        }
    }

    #[test]
    fn a_zero_length_packet_after_a_whole_packet_reply_is_skipped() {
        // The way back reads to end the round, so the replies are 503, 4 and 5 bytes, one
        // whole packet, which a zero-length packet ends.
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftDr);
        let _handle = batch.exchange(BitSequence::repeat(true, 4001));
        batch.enter(TapState::RunTestIdle);
        let stream = [frame(D4, &[0; 500]), frame(D2, &[0]), frame(D2, &[0, 0])].concat();
        let transfers = Transfers([stream, vec![], frame(CMD_JTAG_INIT, &[0])].into());
        let mut dev = on_transport(CH347F_1_20, None, Box::new(transfers));
        dev.pack = Some(Pack::Larger);
        dev.run_jtag_batch(&mut TapState::RunTestIdle, &batch)
            .unwrap();
        // The init after the round reads the zero-length packet first.
        assert_eq!(dev.jtag_init(5).unwrap(), 0);
    }
}
