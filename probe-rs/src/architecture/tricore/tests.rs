//! Tests of the TriCore core control against a model of the OCDS registers.

use std::collections::HashMap;

use super::*;
use crate::architecture::tricore::bus::{BusTransfer, TransferKind, TransferStatus};

const CSFR_BASE: u32 = 0xF881_0000;

/// A model of one core's OCDS registers behind a [`TriCoreBusAccess`].
#[derive(Debug, Default)]
struct FakeBus {
    memory: HashMap<u32, u8>,
    /// Every write, as (address, data).
    writes: Vec<(u32, Vec<u8>)>,
    halted: bool,
    trig_acc: u32,
    access_hw_step: bool,
    resets: usize,
    /// The write addresses of every request.
    requests: Vec<Vec<u32>>,
    /// The number of requests to refuse, as the device does while it reboots.
    refuse: usize,
    /// `DBGSR.EVTSRC` of the last halt.
    evtsrc: u32,
}

impl FakeBus {
    fn new() -> Self {
        let mut bus = Self::default();
        bus.set_word(CSFR_BASE + registers::CSFR_PC as u32, 0x8000_0100);
        bus
    }

    fn word(&self, address: u32) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|i| {
            *self.memory.get(&(address + i as u32)).unwrap_or(&0)
        }))
    }

    fn set_word(&mut self, address: u32, value: u32) {
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.memory.insert(address + i as u32, byte);
        }
    }

    fn csfr(offset: u16) -> u32 {
        CSFR_BASE + offset as u32
    }

    fn write_words(&self) -> Vec<(u32, u32)> {
        self.writes
            .iter()
            .flat_map(|(address, data)| {
                data.chunks(4).enumerate().map(move |(i, chunk)| {
                    let mut word = [0; 4];
                    word[..chunk.len()].copy_from_slice(chunk);
                    (address + 4 * i as u32, u32::from_le_bytes(word))
                })
            })
            .collect()
    }

    fn on_write(&mut self, address: u32, data: &[u8]) {
        self.writes.push((address, data.to_vec()));
        if address == ocds::ACCESS_HW_CTRL && data.len() == 4 {
            let value = u32::from_le_bytes(data.try_into().unwrap());
            self.access_hw_step = value == ocds::ACCESS_HW_STEP_ON;
            return;
        }
        if address == ocds::ACCESS_HW_RUNCMD {
            return;
        }
        let dbgsr = Self::csfr(ocds::DBGSR);
        if address == dbgsr && data.len() == 4 {
            let value = u32::from_le_bytes(data.try_into().unwrap());
            match value {
                ocds::DBGSR_HALT => {
                    // A halt request is an external event.
                    self.halted = true;
                    self.evtsrc = 0;
                }
                ocds::DBGSR_RUN => {
                    let step_armed = self.word(Self::csfr(ocds::TRIG)) == 0x3002;
                    if self.access_hw_step && step_armed {
                        // One instruction, then the step trap halts the core again.
                        let pc = Self::csfr(registers::CSFR_PC);
                        self.set_word(pc, self.word(pc) + 2);
                        self.trig_acc |= 1;
                        self.halted = true;
                        self.evtsrc = 16;
                    } else if self.at_debug() {
                        // Break before make: the core halts at once.
                        self.evtsrc = ocds::EVTSRC_SWEVT;
                    } else {
                        self.halted = false;
                    }
                }
                _ => {}
            }
            return;
        }
        if (dbgsr..dbgsr + 16).contains(&address) {
            // The debug init clears the DBGSR block; that does not change the run state, but it
            // clears EXEVT and CREVT.
            for (i, byte) in data.iter().enumerate() {
                if address + i as u32 >= dbgsr + 4 {
                    self.memory.insert(address + i as u32, *byte);
                }
            }
            return;
        }
        for (i, byte) in data.iter().enumerate() {
            self.memory.insert(address + i as u32, *byte);
        }
    }

    fn on_read(&mut self, address: u32, len: usize) -> Vec<u8> {
        if address == Self::csfr(ocds::DBGSR) {
            let value: u32 = if self.halted {
                0b010 | self.evtsrc << 8
            } else {
                0
            };
            return value.to_le_bytes()[..len].to_vec();
        }
        if address == Self::csfr(ocds::TRIG_ACC) {
            let value = std::mem::take(&mut self.trig_acc);
            return value.to_le_bytes()[..len].to_vec();
        }
        (0..len)
            .map(|i| *self.memory.get(&(address + i as u32)).unwrap_or(&0))
            .collect()
    }

    /// Lets the core run into the trigger of `unit` at the address planted there.
    fn hit_trigger(&mut self, unit: usize) {
        let adr = self.word(Self::csfr(ocds::TRIG + 4 + 8 * unit as u16));
        self.set_word(Self::csfr(registers::CSFR_PC), adr);
        self.trig_acc |= 1 << unit;
        self.halted = true;
        self.evtsrc = 16 + unit as u32;
    }

    /// Lets the core run into a `DEBUG` instruction at `address` (break before make).
    fn hit_debug(&mut self, address: u32) {
        let code = self.bytes(address, 2);
        assert!(
            code == ocds::DEBUG16 || self.bytes(address, 4) == ocds::DEBUG32,
            "no DEBUG instruction at {address:#x}"
        );
        self.set_word(Self::csfr(registers::CSFR_PC), address);
        self.halted = true;
        self.evtsrc = ocds::EVTSRC_SWEVT;
    }

    /// Whether the PC is at a `DEBUG` instruction.
    fn at_debug(&self) -> bool {
        let pc = self.word(Self::csfr(registers::CSFR_PC));
        self.bytes(pc, 2) == ocds::DEBUG16 || self.bytes(pc, 4) == ocds::DEBUG32
    }

    fn bytes(&self, address: u32, len: usize) -> Vec<u8> {
        (0..len as u32)
            .map(|i| *self.memory.get(&(address + i)).unwrap_or(&0))
            .collect()
    }

    fn set_bytes(&mut self, address: u32, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.memory.insert(address + i as u32, *byte);
        }
    }
}

impl TriCoreBusAccess for FakeBus {
    fn execute(&mut self, transfers: &mut [BusTransfer]) -> Result<(), TriCoreError> {
        if self.refuse > 0 {
            self.refuse -= 1;
            for transfer in transfers {
                transfer.status = Some(TransferStatus::Failed(0x44));
            }
            return Ok(());
        }
        self.requests.push(
            transfers
                .iter()
                .filter(|t| !t.is_read())
                .map(|t| t.address)
                .collect(),
        );
        for transfer in transfers {
            match transfer.kind {
                TransferKind::Read { len } => {
                    transfer.data = self.on_read(transfer.address, len);
                }
                TransferKind::Write => {
                    let data = transfer.data.clone();
                    self.on_write(transfer.address, &data);
                }
            }
            transfer.status = Some(TransferStatus::Ok);
        }
        Ok(())
    }

    fn reset(&mut self, _kind: ResetKind) -> Result<(), TriCoreError> {
        self.resets += 1;
        self.halted = true;
        // The boot software leaves its halt-after-reset trigger behind.
        self.set_word(Self::csfr(ocds::TRIG), 0x1022);
        self.trig_acc = 1;
        // The reset clears the halt group routing.
        for core in 0..2 {
            self.set_word(ocds::trc(core), 0);
            self.set_word(CSFR_BASE + 0x2_0000 * core as u32 + 0xFD08, 0);
        }
        Ok(())
    }

    fn jtag_id(&mut self) -> Result<u32, TriCoreError> {
        Ok(0x401D_A083)
    }
}

fn options() -> TriCoreCoreAccessOptions {
    TriCoreCoreAccessOptions {
        core_index: 0,
        csfr_base: CSFR_BASE as u64,
    }
}

fn core<'a>(bus: &'a mut FakeBus, state: &'a mut TriCoreCoreState) -> TriCore<'a> {
    TriCore::new(
        bus,
        state,
        options(),
        sequences::DefaultTriCoreSequence::create(),
    )
}

#[test]
fn halt_writes_dbgsr_and_reports_a_request() {
    let mut bus = FakeBus::new();
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);

    let info = core.halt(Duration::from_millis(100)).unwrap();
    assert_eq!(info.pc, 0x8000_0100);
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Request)
    );
    drop(core);
    assert!(bus.write_words().contains(&(CSFR_BASE + 0xFD00, 6)));
}

#[test]
fn run_uses_the_access_hardware_sequence() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);

    core.run().unwrap();
    assert_eq!(core.status().unwrap(), CoreStatus::Running);
    drop(core);
    // A run leaves the trigger units alone: another session may own them.
    assert!(!state.owns_triggers());
    assert_eq!(
        bus.write_words(),
        [
            (0xF000_0494, 0x8000_003E),
            (CSFR_BASE + 0xFD00, 4),
            (0xF000_0494, 0x8000_003F),
            (CSFR_BASE + 0xFD00, 4),
            (0xF000_049C, 2),
        ]
    );
    // One request, so the debugger of another core cannot run its sequence in between.
    assert!(bus.requests.contains(&vec![
        0xF000_0494,
        CSFR_BASE + 0xFD00,
        0xF000_0494,
        CSFR_BASE + 0xFD00,
        0xF000_049C
    ]));
}

#[test]
fn a_session_leaves_the_triggers_of_a_core_it_only_watches() {
    let mut bus = FakeBus::new();
    // Another session's breakpoint on this core.
    bus.set_word(FakeBus::csfr(ocds::TRIG), ocds::TRIG_EVT_IP);
    bus.set_word(FakeBus::csfr(ocds::TRIG + 4), 0x8000_0400);
    let mut state = TriCoreCoreState::new();
    {
        let mut core = core(&mut bus, &mut state);
        core.status().unwrap();
        core.halt(Duration::from_millis(100)).unwrap();
        core.read_core_reg(SP.id).unwrap();
    }
    assert!(!state.owns_triggers());
    assert_eq!(bus.word(FakeBus::csfr(ocds::TRIG)), ocds::TRIG_EVT_IP);

    // Setting a breakpoint takes the core over and clears what was left there.
    core(&mut bus, &mut state)
        .set_hw_breakpoint(1, 0x8000_0200)
        .unwrap();
    assert!(state.owns_triggers());
    assert_eq!(bus.word(FakeBus::csfr(ocds::TRIG)), 0);
}

#[test]
fn the_step_bracket_is_one_request() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    core(&mut bus, &mut state).step().unwrap();
    assert!(
        bus.requests
            .contains(&vec![0xF000_0494, CSFR_BASE + 0xFD00, 0xF000_0494])
    );
    // No request writes the shared register on its own.
    assert!(
        bus.requests
            .iter()
            .filter(|r| r.contains(&0xF000_0494))
            .all(|r| r.len() == 3)
    );
}

#[test]
fn step_runs_one_instruction_and_restores_breakpoints() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    core.set_hw_breakpoint(3, 0x8000_0200).unwrap();

    let info = core.step().unwrap();
    assert_eq!(info.pc, 0x8000_0102);
    assert_eq!(core.status().unwrap(), CoreStatus::Halted(HaltReason::Step));
    drop(core);

    // The breakpoint is written again after the step sequence reprogrammed the triggers.
    let writes = bus.write_words();
    let last_init = writes
        .iter()
        .rposition(|&(address, value)| address == CSFR_BASE + 0xFD10 && value == 0x2A)
        .unwrap();
    assert!(writes[last_init..].contains(&(CSFR_BASE + 0xF000 + 4 + 8 * 3, 0x8000_0200)));
    assert!(writes[last_init..].contains(&(CSFR_BASE + 0xF000 + 8 * 3, 0x101A)));
    // The step trap accumulator bit was discarded.
    assert_eq!(bus.trig_acc, 0);
}

#[test]
fn a_planted_breakpoint_is_reported_as_hardware_breakpoint() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    {
        let mut core = core(&mut bus, &mut state);
        core.enable_breakpoints(true).unwrap();
        core.set_hw_breakpoint(1, 0x8000_0400).unwrap();
        core.run().unwrap();
        assert_eq!(core.status().unwrap(), CoreStatus::Running);
    }
    bus.hit_trigger(1);
    let mut core = core(&mut bus, &mut state);
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
    );
    // Running again from the breakpoint steps over it first.
    core.run().unwrap();
    assert_eq!(core.status().unwrap(), CoreStatus::Running);
}

#[test]
fn reset_rewrites_the_trigger_units() {
    let mut bus = FakeBus::new();
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);

    core.reset_and_halt(Duration::from_millis(100)).unwrap();
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Request)
    );
    drop(core);
    assert_eq!(bus.resets, 1);
    // The trigger left by the boot software is disabled and its accumulator bit discarded.
    assert_eq!(bus.word(CSFR_BASE + 0xF000), 0);
    assert_eq!(bus.trig_acc, 0);
}

/// CPU1, a peer of the modelled core (CPU0) for the halt group.
const PEER_CSFR_BASE: u32 = 0xF883_0000;
const PEER_DBGSR: u32 = PEER_CSFR_BASE + 0xFD00;

fn group_core<'a>(bus: &'a mut FakeBus, state: &'a mut TriCoreCoreState) -> TriCore<'a> {
    core(bus, state).with_peers([(0, CSFR_BASE), (1, PEER_CSFR_BASE)])
}

#[test]
fn the_halt_group_routes_the_core_and_puts_the_registers_back() {
    let mut bus = FakeBus::new();
    // After a reset CPU0 halts onto line 2, and its break-out bit is set.
    bus.set_word(ocds::trc(0), 0x0000_0102);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    assert!(state.in_halt_group());
    assert_eq!(bus.word(ocds::trc(0)), 0x0070_0107);
    assert_eq!(bus.word(CSFR_BASE + 0xFD08), 0x12);

    group_core(&mut bus, &mut state)
        .set_halt_group(false)
        .unwrap();
    assert!(!state.in_halt_group());
    assert_eq!(bus.word(ocds::trc(0)), 0x0000_0102);
    assert_eq!(bus.word(CSFR_BASE + 0xFD08), 0);
}

#[test]
fn a_step_keeps_the_halt_group_action() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = group_core(&mut bus, &mut state);
    core.set_halt_group(true).unwrap();
    core.step().unwrap();
    drop(core);
    // The debug init of the step clears EXEVT; it is written again.
    assert_eq!(bus.word(CSFR_BASE + 0xFD08), ocds::EXEVT_HALT);
}

#[test]
fn a_halt_by_another_core_is_external() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    core(&mut bus, &mut state).run().unwrap();
    // The break-in halts the core: DBGSR.EVTSRC is 0 (EXEVT).
    bus.halted = true;
    assert_eq!(
        core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Halted(HaltReason::External)
    );
}

#[test]
fn a_run_also_runs_the_members_that_the_group_halted() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();

    // Both cores run, then CPU1 halts (a breakpoint would show EVTSRC 16 + n) and the group
    // halts CPU0.
    bus.set_word(PEER_DBGSR, 0x0001);
    assert_eq!(
        group_core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Running
    );
    bus.set_word(PEER_DBGSR, 0x0003);
    bus.halted = true;
    let mut core = group_core(&mut bus, &mut state);
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::External)
    );
    core.run().unwrap();
    drop(core);

    // CPU1 runs again in the same request as CPU0, before it.
    let dbgsr = CSFR_BASE + 0xFD00;
    assert!(bus.requests.contains(&vec![
        0xF000_0494,
        PEER_DBGSR,
        0xF000_0494,
        PEER_DBGSR,
        0xF000_049C,
        0xF000_0494,
        dbgsr,
        0xF000_0494,
        dbgsr,
        0xF000_049C,
    ]));
}

#[test]
fn joining_while_halted_runs_the_members_it_halted() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    // A debugger attaches to CPU0 and halts it while CPU1, a member, runs.
    bus.halted = true;
    bus.set_word(PEER_DBGSR, 0x0001);
    let mut state = TriCoreCoreState::new();
    let mut core = group_core(&mut bus, &mut state);
    core.set_halt_group(true).unwrap();
    drop(core);
    // The join puts CPU0's halt on the line: CPU1 halts.
    bus.set_word(PEER_DBGSR, 0x0003);
    group_core(&mut bus, &mut state).run().unwrap();
    assert!(bus.write_words().contains(&(PEER_DBGSR, ocds::DBGSR_RUN)));
}

#[test]
fn a_run_leaves_a_member_that_never_ran_halted() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    // CPU1 waits to be started by the application, as after a reset.
    bus.set_word(PEER_DBGSR, 0x0003);
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = group_core(&mut bus, &mut state);
    core.set_halt_group(true).unwrap();
    core.status().unwrap();
    core.run().unwrap();
    drop(core);
    assert!(
        !bus.write_words()
            .iter()
            .any(|&(address, _)| address == PEER_DBGSR)
    );
}

#[test]
fn a_run_also_runs_a_member_halted_after_a_step() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    bus.set_word(PEER_DBGSR, 0x0001);
    group_core(&mut bus, &mut state).status().unwrap();
    // CPU1's debugger stepped it (the step trap in TR0: EVTSRC 16), it is not at a breakpoint.
    bus.set_word(PEER_DBGSR, 0x1003);
    bus.set_word(PEER_CSFR_BASE + 0xFE08, 0x8000_0500);
    bus.halted = true;
    let mut core = group_core(&mut bus, &mut state);
    core.status().unwrap();
    core.run().unwrap();
    drop(core);
    assert!(bus.write_words().contains(&(PEER_DBGSR, ocds::DBGSR_RUN)));
}

#[test]
fn a_halt_request_halts_the_members_that_run() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    bus.set_word(PEER_DBGSR, 0x0001);
    let mut state = TriCoreCoreState::new();
    let mut core = group_core(&mut bus, &mut state);
    core.set_halt_group(true).unwrap();
    core.halt(Duration::from_millis(100)).unwrap();
    drop(core);
    assert!(bus.write_words().contains(&(PEER_DBGSR, ocds::DBGSR_HALT)));
}

#[test]
fn a_run_that_halts_at_once_runs_the_members_again_next_time() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    bus.set_word(PEER_DBGSR, 0x0001);
    group_core(&mut bus, &mut state).status().unwrap();
    bus.set_word(PEER_DBGSR, 0x0003);
    bus.halted = true;
    group_core(&mut bus, &mut state).status().unwrap();

    for _ in 0..2 {
        group_core(&mut bus, &mut state).run().unwrap();
        // CPU0 reaches a breakpoint before any status check sees it run: the group halts CPU1.
        let runs = bus
            .write_words()
            .iter()
            .filter(|&&w| w == (PEER_DBGSR, ocds::DBGSR_RUN))
            .count();
        assert!(runs > 0);
        bus.writes.clear();
        bus.set_word(PEER_DBGSR, 0x0003);
        bus.halted = true;
    }
}

#[test]
fn a_member_that_missed_the_halt_is_halted_when_the_halt_is_seen() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    assert_eq!(
        group_core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Running
    );
    // CPU0 halts while CPU1 still runs: it missed the line (it was resumed just after).
    bus.set_word(PEER_DBGSR, 0x0001);
    bus.halted = true;
    group_core(&mut bus, &mut state).status().unwrap();
    assert!(bus.write_words().contains(&(PEER_DBGSR, ocds::DBGSR_HALT)));
}

#[test]
fn a_member_halted_at_its_breakpoint_stays_halted() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    bus.set_word(PEER_DBGSR, 0x0001);
    group_core(&mut bus, &mut state).status().unwrap();
    // CPU1 halts at the breakpoint in its trigger unit 2: EVTSRC 18.
    bus.set_word(PEER_DBGSR, 0x1203);
    bus.set_word(PEER_CSFR_BASE + 0xF000 + 8 * 2, ocds::TRIG_EVT_IP);
    bus.set_word(PEER_CSFR_BASE + 0xF000 + 8 * 2 + 4, 0x8000_0600);
    bus.set_word(PEER_CSFR_BASE + 0xFE08, 0x8000_0600);
    bus.halted = true;
    let mut core = group_core(&mut bus, &mut state);
    core.status().unwrap();
    core.run().unwrap();
    drop(core);
    assert!(
        !bus.write_words()
            .iter()
            .any(|&(address, _)| address == PEER_DBGSR)
    );
}

#[test]
fn a_reset_routes_the_core_again() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    group_core(&mut bus, &mut state)
        .reset_and_halt(Duration::from_millis(100))
        .unwrap();
    // The reset cleared the routing: this session's core is routed again, CPU1 (the member of
    // another session, or one left by a session that ended without cleaning up) is not.
    assert_eq!(bus.word(ocds::trc(0)), ocds::TRC_HALT_GROUP);
    assert_eq!(bus.word(CSFR_BASE + 0xFD08), ocds::EXEVT_HALT);
    assert_eq!(bus.word(ocds::trc(1)), 0);
}

#[test]
fn a_member_routes_itself_again_after_a_reset_by_another_session() {
    let mut bus = FakeBus::new();
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    // Another session resets the device.
    bus.reset(ResetKind::ResetAndHalt).unwrap();
    assert_eq!(bus.word(ocds::trc(0)), 0);
    group_core(&mut bus, &mut state).status().unwrap();
    assert_eq!(bus.word(ocds::trc(0)), ocds::TRC_HALT_GROUP);
    assert_eq!(bus.word(CSFR_BASE + 0xFD08), ocds::EXEVT_HALT);
}

#[test]
fn the_status_rides_out_a_reboot() {
    let mut bus = FakeBus::new();
    // Another session resets the device: the next accesses fail.
    bus.refuse = 3;
    let mut state = TriCoreCoreState::new();
    assert_eq!(
        core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Running
    );
}

#[test]
fn registers_are_read_from_the_csfr_window() {
    let mut bus = FakeBus::new();
    bus.set_word(CSFR_BASE + 0xFFA8, 0x7000_C000);
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    let sp = core.read_core_reg(SP.id).unwrap();
    assert_eq!(sp, RegisterValue::U32(0x7000_C000));
    // Register writes need a halted core.
    assert!(core.write_core_reg(SP.id, RegisterValue::U32(0)).is_err());
}

#[test]
fn local_scratchpad_addresses_use_the_core_alias() {
    assert_eq!(translate_local_address(0, 0xD000_1000), 0x7000_1000);
    assert_eq!(translate_local_address(1, 0xD000_1000), 0x6000_1000);
    assert_eq!(translate_local_address(2, 0xC000_0010), 0x5010_0010);
    assert_eq!(translate_local_address(1, 0x8000_0000), 0x8000_0000);

    let mut bus = FakeBus::new();
    bus.set_word(0x6000_0010, 0xCAFE_F00D);
    let mut state = TriCoreCoreState::new();
    let mut core = TriCore::new(
        &mut bus,
        &mut state,
        TriCoreCoreAccessOptions {
            core_index: 1,
            csfr_base: 0xF883_0000,
        },
        sequences::DefaultTriCoreSequence::create(),
    );
    assert_eq!(core.read_word_32(0xD000_0010).unwrap(), 0xCAFE_F00D);
}

/// Code in the PSPR of CPU0 (the modelled core): a 32-bit `st.w` and a 16-bit `mov.aa`.
const PSPR_ST_W: u32 = 0x7010_0004;
const ST_W: [u8; 4] = [0x59, 0xE4, 0xFC, 0xFF];
const PSPR_MOV_AA: u32 = 0x7010_0010;
const MOV_AA: [u8; 2] = [0x40, 0xAE];

fn pspr_bus() -> FakeBus {
    let mut bus = FakeBus::new();
    bus.set_bytes(PSPR_ST_W, &ST_W);
    bus.set_bytes(PSPR_MOV_AA, &MOV_AA);
    bus.halted = true;
    bus
}

#[test]
fn a_breakpoint_in_the_pspr_is_a_debug_instruction() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    core.set_hw_breakpoint(0, PSPR_ST_W as u64).unwrap();
    // The local alias of the PSPR as well.
    core.set_hw_breakpoint(1, 0xC000_0010).unwrap();
    assert_eq!(
        core.hw_breakpoints().unwrap()[..2],
        [Some(PSPR_ST_W as u64), Some(0xC000_0010)]
    );
    // Reads show the program, not the breakpoints.
    let mut code = [0; 0x10];
    core.read(0x7010_0004, &mut code).unwrap();
    assert_eq!(code[..4], ST_W);
    assert_eq!(code[12..14], MOV_AA);
    drop(core);

    assert_eq!(bus.bytes(PSPR_ST_W, 4), ocds::DEBUG32);
    assert_eq!(bus.bytes(PSPR_MOV_AA, 2), ocds::DEBUG16);
    // A DEBUG instruction halts the core, and no trigger unit is used.
    assert_eq!(bus.word(FakeBus::csfr(ocds::SWEVT)), ocds::SWEVT_HALT);
    assert!(
        (0..ocds::TRIGGER_UNITS)
            .all(|unit| bus.word(FakeBus::csfr(ocds::TRIG + 8 * unit as u16)) == 0)
    );

    // Clearing puts the instructions back.
    let mut again = self::core(&mut bus, &mut state);
    again.clear_hw_breakpoint(0).unwrap();
    again.clear_hw_breakpoint(1).unwrap();
    drop(again);
    assert_eq!(bus.bytes(PSPR_ST_W, 4), ST_W);
    assert_eq!(bus.bytes(PSPR_MOV_AA, 2), MOV_AA);
}

#[test]
fn software_breakpoints_go_beyond_the_trigger_units() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    // A breakpoint in the PSPR in slot 0: it leaves the trigger units to the flash.
    core.set_hw_breakpoint(0, 0x7010_0100).unwrap();
    for i in 0..ocds::TRIGGER_UNITS {
        core.set_hw_breakpoint(1 + i, 0x8000_0200 + 4 * i as u64)
            .unwrap();
    }
    for i in 1..20 {
        core.set_hw_breakpoint(ocds::TRIGGER_UNITS + i, 0x7010_0100 + 4 * i as u64)
            .unwrap();
    }
    // The flash has no room left, and the slot stays free.
    assert!(core.set_hw_breakpoint(30, 0x8000_0300).is_err());
    let slots = core.hw_breakpoints().unwrap();
    assert_eq!(slots.len(), ocds::BREAKPOINT_SLOTS);
    assert_eq!(slots[30], None);
    assert_eq!(slots.iter().flatten().count(), ocds::TRIGGER_UNITS + 20);
    drop(core);
    for unit in 0..ocds::TRIGGER_UNITS {
        assert_eq!(
            bus.word(FakeBus::csfr(ocds::TRIG + 8 * unit as u16)),
            ocds::TRIG_EVT_IP
        );
    }
    assert_eq!(bus.bytes(0x7010_0100, 2), ocds::DEBUG16);
    assert_eq!(bus.bytes(0x7010_0100 + 4 * 19, 2), ocds::DEBUG16);
}

#[test]
fn a_software_breakpoint_halt_is_reported_and_stepped_over() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    {
        let mut core = core(&mut bus, &mut state);
        core.enable_breakpoints(true).unwrap();
        core.set_hw_breakpoint(0, PSPR_MOV_AA as u64).unwrap();
        core.run().unwrap();
    }
    bus.hit_debug(PSPR_MOV_AA);
    assert_eq!(
        core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Software))
    );
    let writes_before = bus.writes.len();
    // Running from the breakpoint steps the instruction under it first.
    let mut core = core(&mut bus, &mut state);
    core.run().unwrap();
    assert_eq!(core.status().unwrap(), CoreStatus::Running);
    drop(core);

    let writes = &bus.writes[writes_before..];
    let restored = writes
        .iter()
        .position(|w| *w == (PSPR_MOV_AA, MOV_AA.to_vec()))
        .unwrap();
    let step = writes
        .iter()
        .position(|w| w.0 == ocds::ACCESS_HW_CTRL)
        .unwrap();
    let planted = writes
        .iter()
        .rposition(|w| *w == (PSPR_MOV_AA, ocds::DEBUG16.to_vec()))
        .unwrap();
    assert!(restored < step && step < planted);
    assert_eq!(bus.bytes(PSPR_MOV_AA, 2), ocds::DEBUG16);
}

#[test]
fn a_run_that_steps_onto_the_next_software_breakpoint_halts_there() {
    let mut bus = pspr_bus();
    bus.set_bytes(PSPR_MOV_AA + 2, &[0x20, 0x08]);
    bus.set_word(FakeBus::csfr(registers::CSFR_PC), PSPR_MOV_AA);
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    core.set_hw_breakpoint(0, PSPR_MOV_AA as u64).unwrap();
    core.set_hw_breakpoint(1, PSPR_MOV_AA as u64 + 2).unwrap();
    // The step over the first breakpoint ends on the second: the run halts at once, normally.
    core.run().unwrap();
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Software))
    );
    assert_eq!(core.core_info().unwrap().pc, PSPR_MOV_AA as u64 + 2);
}

#[test]
fn code_written_over_a_software_breakpoint_keeps_it() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    core.set_hw_breakpoint(0, PSPR_ST_W as u64).unwrap();
    // The program is loaded again, with other code there.
    core.write(0x7010_0000, &[0x11; 8]).unwrap();
    assert_eq!(core.read_word_32(PSPR_ST_W as u64).unwrap(), 0x1111_1111);
    drop(core);
    assert_eq!(bus.bytes(PSPR_ST_W, 4), ocds::DEBUG32);

    self::core(&mut bus, &mut state)
        .clear_hw_breakpoint(0)
        .unwrap();
    assert_eq!(bus.bytes(PSPR_ST_W, 4), [0x11; 4]);
}

#[test]
fn disabled_breakpoints_leave_the_code() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    {
        let mut core = core(&mut bus, &mut state);
        core.enable_breakpoints(true).unwrap();
        core.set_hw_breakpoint(0, PSPR_MOV_AA as u64).unwrap();
        core.enable_breakpoints(false).unwrap();
    }
    assert_eq!(bus.bytes(PSPR_MOV_AA, 2), MOV_AA);
    core(&mut bus, &mut state).enable_breakpoints(true).unwrap();
    assert_eq!(bus.bytes(PSPR_MOV_AA, 2), ocds::DEBUG16);
}

#[test]
fn a_debug_instruction_of_the_program_is_skipped() {
    let mut bus = FakeBus::new();
    bus.set_bytes(0x7010_0020, &ocds::DEBUG16);
    bus.set_word(FakeBus::csfr(registers::CSFR_PC), 0x7010_0020);
    bus.halted = true;
    bus.evtsrc = ocds::EVTSRC_SWEVT;
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Software))
    );
    // Stepping moves past it, without running the core.
    assert_eq!(core.step().unwrap().pc, 0x7010_0022);
    drop(core);
    assert!(!bus.writes.iter().any(|w| w.0 == ocds::ACCESS_HW_CTRL));
}

#[test]
fn a_member_halted_at_a_software_breakpoint_stays_halted() {
    let mut bus = FakeBus::new();
    bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
    let mut state = TriCoreCoreState::new();
    group_core(&mut bus, &mut state)
        .set_halt_group(true)
        .unwrap();
    bus.set_word(PEER_DBGSR, 0x0001);
    group_core(&mut bus, &mut state).status().unwrap();
    // CPU1 halts at a DEBUG instruction. The break-in from the group line that follows
    // overwrites EVTSRC (0), only PREVSUSP shows the halt: the code at the PC tells.
    bus.set_word(PEER_DBGSR, 0x0043);
    bus.set_word(PEER_CSFR_BASE + 0xFE08, 0xC000_0010);
    bus.set_bytes(0x6010_0010, &ocds::DEBUG16);
    bus.halted = true;
    let mut core = group_core(&mut bus, &mut state);
    core.status().unwrap();
    core.run().unwrap();
    drop(core);
    assert!(
        !bus.write_words()
            .iter()
            .any(|&(address, _)| address == PEER_DBGSR)
    );
}

#[test]
fn a_software_breakpoint_in_the_halt_group_is_a_breakpoint() {
    let mut bus = pspr_bus();
    let mut state = TriCoreCoreState::new();
    {
        let mut core = group_core(&mut bus, &mut state);
        core.set_halt_group(true).unwrap();
        core.enable_breakpoints(true).unwrap();
        core.set_hw_breakpoint(0, PSPR_MOV_AA as u64).unwrap();
        core.run().unwrap();
    }
    // The halt comes back as break-in through the group line: EVTSRC is 0.
    bus.hit_debug(PSPR_MOV_AA);
    bus.evtsrc = 0;
    assert_eq!(
        group_core(&mut bus, &mut state).status().unwrap(),
        CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Software))
    );
}

fn trig_evt(unit: usize) -> u32 {
    FakeBus::csfr(ocds::TRIG + 8 * unit as u16)
}

fn trig_adr(unit: usize) -> u32 {
    FakeBus::csfr(ocds::TRIG + 4 + 8 * unit as u16)
}

#[test]
fn a_watchpoint_takes_the_highest_free_trigger_units() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    // One byte: a single unit, stores only.
    core.set_watchpoint(0x7000_A000, 1, WatchpointAccess::Write)
        .unwrap();
    // Four bytes: an even and odd pair, loads and stores.
    core.set_watchpoint(0x7000_A004, 4, WatchpointAccess::ReadWrite)
        .unwrap();
    assert_eq!(core.watchpoints().unwrap(), [0x7000_A000, 0x7000_A004]);
    drop(core);

    assert_eq!(bus.word(trig_adr(7)), 0x7000_A000);
    assert_eq!(bus.word(trig_evt(7)), 0x0800_0032);
    // Units 6 and 7 are not a free pair: 4 and 5 hold the range.
    assert_eq!(bus.word(trig_adr(4)), 0x7000_A004);
    assert_eq!(bus.word(trig_evt(4)), 0x1800_2032);
    assert_eq!(bus.word(trig_adr(5)), 0x7000_A008);
    assert_eq!(bus.word(trig_evt(5)), 0);

    let mut core = self::core(&mut bus, &mut state);
    core.clear_watchpoint(0x7000_A004).unwrap();
    drop(core);
    assert_eq!(bus.word(trig_evt(4)), 0);
    assert_eq!(bus.word(trig_evt(7)), 0x0800_0032);
}

#[test]
fn breakpoints_and_watchpoints_share_the_trigger_units() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.enable_breakpoints(true).unwrap();
    for slot in 0..7 {
        core.set_hw_breakpoint(slot, 0x8000_0200 + 4 * slot as u64)
            .unwrap();
    }
    // No even and odd pair is left, a single unit is.
    let error = core
        .set_watchpoint(0x7000_A004, 4, WatchpointAccess::Write)
        .unwrap_err();
    assert!(
        error.to_string().contains("No free trigger unit"),
        "{error}"
    );
    core.set_watchpoint(0x7000_A004, 1, WatchpointAccess::Write)
        .unwrap();
    // And then no unit is left for a breakpoint in the flash.
    assert!(core.set_hw_breakpoint(7, 0x8000_0300).is_err());
}

#[test]
fn a_watchpoint_halt_is_reported_and_survives_a_step() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    let mut state = TriCoreCoreState::new();
    {
        let mut core = core(&mut bus, &mut state);
        core.set_watchpoint(0x7000_A004, 4, WatchpointAccess::Write)
            .unwrap();
        core.run().unwrap();
    }
    // The store halts the core after it (break after make): the accumulator bit of unit 6.
    bus.trig_acc |= 1 << 6;
    bus.halted = true;
    bus.evtsrc = 16 + 6;
    let mut core = core(&mut bus, &mut state);
    assert_eq!(
        core.status().unwrap(),
        CoreStatus::Halted(HaltReason::Watchpoint)
    );
    // The step reprograms the trigger window; the watchpoint is armed again after it.
    core.step().unwrap();
    drop(core);
    assert_eq!(bus.word(trig_evt(6)), 0x0800_2032);
    assert_eq!(bus.word(trig_adr(7)), 0x7000_A008);
}

#[test]
fn a_member_halted_at_a_watchpoint_stays_halted() {
    for peer_dbgsr in [
        // Its own trigger (EVTSRC 16 + 6, SUSP).
        0x1613, // The break-in of the group right after it (EVTSRC 0, PREVSUSP).
        0x0043,
    ] {
        let mut bus = FakeBus::new();
        bus.set_word(ocds::trc(1), ocds::TRC_HALT_GROUP);
        let mut state = TriCoreCoreState::new();
        group_core(&mut bus, &mut state)
            .set_halt_group(true)
            .unwrap();
        bus.set_word(PEER_DBGSR, 0x0001);
        group_core(&mut bus, &mut state).status().unwrap();
        bus.set_word(PEER_DBGSR, peer_dbgsr);
        bus.halted = true;
        let mut core = group_core(&mut bus, &mut state);
        core.status().unwrap();
        core.run().unwrap();
        drop(core);
        assert!(
            !bus.write_words()
                .iter()
                .any(|&(address, value)| address == PEER_DBGSR && value == ocds::DBGSR_RUN),
            "DBGSR {peer_dbgsr:#x}"
        );
    }
}

const CCTRL: u32 = CSFR_BASE + registers::CSFR_CCTRL as u32;

#[test]
fn a_perf_window_counts_between_two_triggers_and_puts_cctrl_back() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    // M1 configured by the application, counters disabled.
    bus.set_word(CCTRL, 0x0000_0004);
    bus.set_word(CSFR_BASE + registers::CSFR_ICNT as u32, 1234);
    let mut state = TriCoreCoreState::new();
    let mut core = core(&mut bus, &mut state);
    core.set_perf_window(Some((0x8021_C000, 0x8021_C024)))
        .unwrap();
    // The window takes the two highest units: a watchpoint range finds no pair there.
    core.set_watchpoint(0x7000_A004, 4, WatchpointAccess::Write)
        .unwrap();
    core.step().unwrap();
    drop(core);

    assert_eq!(bus.word(trig_adr(6)), 0x8021_C000);
    assert_eq!(bus.word(trig_evt(6)), ocds::TRIG_EVT_PERF_START);
    assert_eq!(bus.word(trig_adr(7)), 0x8021_C024);
    assert_eq!(bus.word(trig_evt(7)), ocds::TRIG_EVT_PERF_STOP);
    assert_eq!(bus.word(trig_evt(4)), 0x0800_2032);
    // Cleared counters, task mode, enabled, M1 kept.
    assert_eq!(bus.word(CSFR_BASE + registers::CSFR_ICNT as u32), 0);
    assert_eq!(bus.word(CCTRL), 0x0000_0007);

    self::core(&mut bus, &mut state)
        .set_perf_window(None)
        .unwrap();
    assert_eq!(bus.word(trig_evt(6)), 0);
    assert_eq!(bus.word(trig_evt(7)), 0);
    assert_eq!(bus.word(CCTRL), 0x0000_0004);
}

const ICR: u32 = CSFR_BASE + registers::CSFR_ICR as u32;

/// The (index, value) of every ICR write.
fn icr_writes(bus: &FakeBus) -> Vec<(usize, u32)> {
    bus.write_words()
        .into_iter()
        .enumerate()
        .filter(|&(_, (address, _))| address == ICR)
        .map(|(i, (_, value))| (i, value))
        .collect()
}

#[test]
fn a_step_runs_with_interrupts_disabled() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    bus.set_word(ICR, 0x0000_8000);
    let mut state = TriCoreCoreState::new();
    core(&mut bus, &mut state).step().unwrap();

    // ICR.IE off before the run bracket of the step, on again after it.
    let writes = bus.write_words();
    let step = writes
        .iter()
        .position(|&(address, value)| address == FakeBus::csfr(ocds::DBGSR) && value == 4)
        .unwrap();
    let icr = icr_writes(&bus);
    assert_eq!(icr.len(), 2);
    assert!(icr[0].0 < step && icr[0].1 == 0);
    assert!(icr[1].0 > step && icr[1].1 == 0x8000);
    assert_eq!(bus.word(ICR), 0x8000);
}

#[test]
fn a_step_over_an_instruction_that_changes_ie_leaves_icr_alone() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    bus.set_word(ICR, 0x0000_8000);
    // DISABLE at the PC.
    bus.set_bytes(0x8000_0100, &[0x0D, 0x00, 0x40, 0x03]);
    let mut state = TriCoreCoreState::new();
    core(&mut bus, &mut state).step().unwrap();
    assert!(icr_writes(&bus).is_empty());
}

#[test]
fn steps_can_leave_interrupts_enabled() {
    let mut bus = FakeBus::new();
    bus.halted = true;
    bus.set_word(ICR, 0x0000_8000);
    let mut state = TriCoreCoreState::new();
    state.set_mask_step_interrupts(false);
    core(&mut bus, &mut state).step().unwrap();
    assert!(icr_writes(&bus).is_empty());
    // With interrupts disabled there is nothing to mask either.
    bus.writes.clear();
    bus.set_word(ICR, 0);
    let mut state = TriCoreCoreState::new();
    core(&mut bus, &mut state).step().unwrap();
    assert!(icr_writes(&bus).is_empty());
}

#[test]
fn instructions_that_change_ie_are_recognized() {
    let yes: [&[u8]; 9] = [
        &[0x0D, 0x00, 0x00, 0x03], // enable
        &[0x0D, 0x00, 0x40, 0x03], // disable
        &[0x0D, 0x03, 0xC0, 0x03], // disable %d3
        &[0x0D, 0x05, 0x80, 0x03], // restore %d5
        &[0xE0, 0x05],             // bisr 5
        &[0x00, 0x80],             // rfe
        &[0x0D, 0x00, 0x40, 0x01], // rfm
        &[0xCD, 0xC4, 0xE2, 0x0F], // mtcr $icr, %d4
        &[0x0D, 0x00, 0x80, 0x01], // rfe (32 bit)
    ];
    for code in yes {
        assert!(ocds::changes_interrupt_enable(code), "{code:02x?}");
    }
    let no: [&[u8]; 5] = [
        &[0x4D, 0xC0, 0xE2, 0x4F], // mfcr %d4, $icr
        &[0xAD, 0x30, 0x80, 0x00], // syscall 3
        &[0x00, 0x90],             // ret
        &[0x40, 0xAE],             // mov.aa
        &[0x59, 0xE4, 0xFC, 0xFF], // st.w
    ];
    for code in no {
        assert!(!ocds::changes_interrupt_enable(code), "{code:02x?}");
    }
}

#[test]
fn only_the_own_pspr_takes_software_breakpoints() {
    assert_eq!(own_pspr_address(0, 0x7010_0010), Some(0x7010_0010));
    assert_eq!(own_pspr_address(0, 0xC000_0010), Some(0x7010_0010));
    assert_eq!(own_pspr_address(1, 0xC000_0010), Some(0x6010_0010));
    // The PSPR of another core, the DSPR, the LMU and the flash.
    assert_eq!(own_pspr_address(1, 0x7010_0010), None);
    assert_eq!(own_pspr_address(0, 0x7000_0010), None);
    assert_eq!(own_pspr_address(0, 0xB000_0010), None);
    assert_eq!(own_pspr_address(0, 0x8000_0010), None);
}
