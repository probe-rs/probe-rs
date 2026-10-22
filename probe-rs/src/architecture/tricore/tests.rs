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
            // The debug init clears the DBGSR block; that does not change the run state.
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
    TriCore::new(bus, state, options(), sequences::DefaultTriCoreSequence::create())
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
    assert_eq!(
        bus.write_words(),
        vec![
            (0xF000_0494, 0x8000_003E),
            (CSFR_BASE + 0xFD00, 4),
            (0xF000_0494, 0x8000_003F),
            (CSFR_BASE + 0xFD00, 4),
            (0xF000_049C, 2),
        ]
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
