//! Sequences for AM261x devices.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::{Error, MemoryInterface, Session};

const MSS_CTRL: u64 = 0x50D0_0000;
const R5SS0_STATUS_REG: u64 = MSS_CTRL + 0x64;
const R5SS0_STATUS_REG_LOCK_STEP: u32 = 1 << 8;
const LOCK0_KICK0: u64 = MSS_CTRL + 0x1008;
const LOCK0_KICK1: u64 = MSS_CTRL + 0x100C;
const KICK0_UNLOCK: u32 = 0x0123_4567;
const KICK1_UNLOCK: u32 = 0x0FED_CBA8;

const MEM_INIT_TIMEOUT: Duration = Duration::from_millis(100);

/// A TCM of R5SS0, with its `MEM_INIT`, `MEM_INIT_DONE` and `MEM_INIT_STATUS` registers at
/// `mem_init`, `+ 4` and `+ 8`.
struct Tcm {
    name: &'static str,
    mem_init: u64,
}

const TCMS: [Tcm; 2] = [
    Tcm {
        name: "ATCM",
        mem_init: MSS_CTRL + 0x300,
    },
    Tcm {
        name: "BTCM",
        mem_init: MSS_CTRL + 0x30C,
    },
];

impl Tcm {
    /// Zeroes the TCM and its ECC (TRM 6.1.3.2.2.1). MSS_CTRL must be unlocked.
    fn init(&self, memory: &mut impl MemoryInterface) -> Result<(), Error> {
        let done = self.mem_init + 4;
        let status = self.mem_init + 8;

        wait_for(memory, status, 0)?;
        memory.write_word_32(done, 1)?;
        memory.write_word_32(self.mem_init, 1)?;
        wait_for(memory, done, 1)?;
        memory.write_word_32(done, 1)?;

        tracing::debug!("Initialized R5SS0 {}", self.name);
        Ok(())
    }
}

fn wait_for(memory: &mut impl MemoryInterface, address: u64, value: u32) -> Result<(), Error> {
    let start = Instant::now();
    while memory.read_word_32(address)? & 1 != value {
        if start.elapsed() > MEM_INIT_TIMEOUT {
            return Err(Error::Timeout);
        }
    }
    Ok(())
}

/// AM261x support.
///
/// DevBoot skips the ROM memory init, so the TCM ECC is uninitialized. Like the TI SBL, both TCMs
/// are initialized before a RAM image is loaded, so the application can enable ECC checking
/// (`ACTLR.ATCMPCEN`, `B0TCMPCEN`, `B1TCMPCEN`).
///
/// The TRM requires checking to be enabled before TCM data is written. Measured on an AM2612, word
/// writes store valid ECC with checking disabled, and the core writes the image in whole words
/// (sub-word writes are read-modify-write), so enabling checking is left to the application.
#[derive(Debug)]
pub struct AM261;

impl AM261 {
    /// Create the sequencer for the AM261x family.
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl ArmDebugSequence for AM261 {
    fn prepare_loading_to_ram(&self, session: &mut Session) -> Result<(), Error> {
        let mut core = session.core(0)?;

        // The init covers the TCMs of both cores, in dual-core mode the other core may use its own.
        if core.read_word_32(R5SS0_STATUS_REG)? & R5SS0_STATUS_REG_LOCK_STEP == 0 {
            tracing::warn!("R5SS0 is not in lockstep mode, skipping the TCM init");
            return Ok(());
        }

        core.write_word_32(LOCK0_KICK0, KICK0_UNLOCK)?;
        core.write_word_32(LOCK0_KICK1, KICK1_UNLOCK)?;
        let result = TCMS.iter().try_for_each(|tcm| tcm.init(&mut core));
        core.write_word_32(LOCK0_KICK1, 0)?;
        core.write_word_32(LOCK0_KICK0, 0)?;
        result
    }
}
