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
                ocds::DBGSR_HALT => self.halted = true,
                ocds::DBGSR_RUN => {
                    let step_armed = self.word(Self::csfr(ocds::TRIG)) == 0x3002;
                    if self.access_hw_step && step_armed {
                        // One instruction, then the step trap halts the core again.
                        let pc = Self::csfr(registers::CSFR_PC);
                        self.set_word(pc, self.word(pc) + 2);
                        self.trig_acc |= 1;
                        self.halted = true;
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
            let value: u32 = if self.halted { 0b010 } else { 0 };
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

    // CPU1 runs again in the same request as CPU0.
    let dbgsr = CSFR_BASE + 0xFD00;
    assert!(bus.requests.contains(&vec![
        0xF000_0494,
        dbgsr,
        0xF000_0494,
        dbgsr,
        0xF000_049C,
        0xF000_0494,
        PEER_DBGSR,
        0xF000_0494,
        PEER_DBGSR,
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
