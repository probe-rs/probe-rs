//! Sequences for ARMv6 STM32s: STM32F0, STM32G0, STM32L0.
//!
//! This covers devices where DBGMCU is at 0x40015800 and has the DBG_STANDBY and DBG_STOP bits.

use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        armv6m::{Demcr, Dhcsr},
        core::Dfsr,
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_reset_catch_clear},
    },
};

/// Supported families for custom sequences on ARMv6 STM32 devices.
#[derive(Debug)]
pub enum Stm32Armv6Family {
    /// STM32F0 family
    F0,

    /// STM32L0 family
    L0,

    /// STM32G0 family
    G0,
}

/// What attach found, for detach to put back.
#[derive(Debug, Clone, Copy)]
struct Saved {
    demcr: u32,
    cr: CrState,
    /// DBGEN before this sequence turned it on, once it has.
    dbgen_before: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CrState {
    /// Nothing to put back: the bits were set already, or nothing was written.
    Untouched,
    /// The stop and standby bits are to be set once the core is halted.
    Pending,
    /// DBGMCU CR held `before` and this sequence wrote `written`.
    Written { before: u32, written: u32 },
}

/// Marker structure for ARMv6 STM32 devices.
#[derive(Debug)]
pub struct Stm32Armv6 {
    family: Stm32Armv6Family,
    /// The state attach found, for detach to restore.
    saved: Mutex<Option<Saved>>,
}

impl Stm32Armv6 {
    /// Create the sequencer for ARMv6 STM32 devices.
    pub fn create(family: Stm32Armv6Family) -> Arc<Self> {
        Arc::new(Self {
            family,
            saved: Mutex::new(None),
        })
    }
}

/// The base address of the RCC peripheral
const RCC: u64 = 0x40021000;

/// How long to wait for the core to halt before giving up on changing DBGMCU CR.
const HALT_TIMEOUT: Duration = Duration::from_millis(100);

mod dbgmcu {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const DBGMCU: u64 = 0x40015800;

    bitfield! {
        /// The control register (CR) of the DBGMCU. This register is described in "RM0360: STM32F0
        /// family reference manual" section 26.9.3.
        pub struct Control(pub u32);
        impl Debug;

        pub u8, dbg_standby, enable_standby_debug: 2;
        pub u8, dbg_stop, enable_stop_debug: 1;
    }

    impl Control {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x04;

        /// Read the control register from memory.
        pub fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(DBGMCU + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(DBGMCU + Self::ADDRESS, self.0)
        }
    }
}

/// What `halt` found and did.
enum Halt {
    /// The core was halted already and stays so.
    Already,
    /// The core ran and is halted now; DHCSR and DFSR as they were, for `resume`.
    ByUs { dhcsr: Dhcsr, dfsr: Dfsr },
    /// The core runs and did not halt, or is held in reset.
    Failed,
}

impl Stm32Armv6 {
    /// The RCC enable register with DBGEN, and the bit's position: bit 22 of APB2ENR on F0 and L0,
    /// bit 27 of APBENR1 on G0.
    fn dbgen_register(&self) -> (u64, u32) {
        match self.family {
            Stm32Armv6Family::F0 => (RCC + 0x18, 22),
            Stm32Armv6Family::L0 => (RCC + 0x34, 22),
            Stm32Armv6Family::G0 => (RCC + 0x3c, 27),
        }
    }

    /// Sets DBGEN to `on` and returns what it was. The RCC register is shared with the firmware:
    /// only with the core halted, and only the DBGEN bit changes.
    fn set_dbgen(&self, memory: &mut dyn ArmMemoryInterface, on: bool) -> Result<bool, ArmError> {
        let (address, bit) = self.dbgen_register();
        let enr = memory.read_word_32(address)?;
        let was = enr & 1 << bit != 0;
        if was != on {
            memory.write_word_32(address, enr ^ 1 << bit)?;
        }
        Ok(was)
    }

    /// Sets the stop and standby debug bits if attach left that pending. Writes to DBGMCU CR are
    /// ignored without DBGEN (seen on STM32G0B1), so DBGEN is turned on and stays on for the
    /// session, as before this sequence restored it. Only with the core halted: the firmware may
    /// write DBGMCU CR and the RCC register too.
    fn apply_cr(&self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        if !matches!(
            *self.saved.lock().unwrap(),
            Some(Saved {
                cr: CrState::Pending,
                ..
            })
        ) {
            return Ok(());
        }
        let dbgen_before = self.set_dbgen(memory, true)?;
        self.update(|saved| {
            saved.dbgen_before.get_or_insert(dbgen_before);
        });
        let mut cr = dbgmcu::Control::read(memory)?;
        let before = cr.0;
        cr.enable_standby_debug(true);
        cr.enable_stop_debug(true);
        let state = if cr.0 == before {
            CrState::Untouched
        } else {
            cr.write(memory)?;
            CrState::Written {
                before,
                written: cr.0,
            }
        };
        self.update(|saved| saved.cr = state);
        Ok(())
    }

    /// Turns DBGEN on again if this sequence turned it on before, with the core halted.
    fn dbgen_on_again(&self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        if matches!(
            *self.saved.lock().unwrap(),
            Some(Saved {
                dbgen_before: Some(_),
                ..
            })
        ) {
            self.set_dbgen(memory, true)?;
        }
        Ok(())
    }

    fn update(&self, change: impl FnOnce(&mut Saved)) {
        if let Some(saved) = self.saved.lock().unwrap().as_mut() {
            change(saved);
        }
    }

    /// Puts back what attach changed, with the core halted: DBGMCU CR if it still holds what was
    /// written (else the firmware wrote it), DBGEN and DEMCR. Each one is tried; the first error
    /// is returned.
    fn restore(&self, memory: &mut dyn ArmMemoryInterface, saved: Saved) -> Result<(), ArmError> {
        let mut result = Ok(());
        if let CrState::Written { before, written } = saved.cr {
            result = self.set_dbgen(memory, true).and_then(|_| {
                if dbgmcu::Control::read(memory)?.0 == written {
                    dbgmcu::Control(before).write(memory)?;
                }
                Ok(())
            });
        }
        if let Some(dbgen) = saved.dbgen_before {
            result = result.and(self.set_dbgen(memory, dbgen).map(|_| ()));
        }
        result.and(memory.write_word_32(Demcr::get_mmio_address(), saved.demcr))
    }
}

/// Halts the core if it runs, so that it cannot write a register while we do.
fn halt(memory: &mut dyn ArmMemoryInterface) -> Result<Halt, ArmError> {
    let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
    if dhcsr.s_halt() {
        return Ok(Halt::Already);
    }
    // S_RESET_ST is cleared by a read. Set on two reads in a row, the core is most likely held in
    // reset (attach under reset): it cannot halt, and `reset_catch_clear` gets another chance.
    if dhcsr.s_reset_st() && Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?).s_reset_st() {
        return Ok(Halt::Failed);
    }
    let dfsr = Dfsr(memory.read_word_32(Dfsr::get_mmio_address())?);
    let halted = Halt::ByUs { dhcsr, dfsr };

    let mut request = Dhcsr(0);
    request.enable_write();
    request.set_c_debugen(true);
    request.set_c_halt(true);
    memory.write_word_32(Dhcsr::get_mmio_address(), request.into())?;

    let start = Instant::now();
    loop {
        match memory.read_word_32(Dhcsr::get_mmio_address()) {
            Ok(value) if Dhcsr(value).s_halt() => return Ok(halted),
            Ok(_) if start.elapsed() < HALT_TIMEOUT => thread::sleep(Duration::from_millis(1)),
            Ok(_) => {
                tracing::warn!(
                    "Core did not halt, DBGMCU and RCC are not changed: stop and standby debug may \
                     be off, connect under reset to have them set"
                );
                resume(memory, &halted)?;
                return Ok(Halt::Failed);
            }
            Err(error) => {
                // Don't leave the halt request behind.
                let _ = resume(memory, &halted);
                return Err(error);
            }
        }
    }
}

/// Lets a core halted by `halt` run again as it ran before: C_DEBUGEN and C_MASKINTS as they
/// were, and the DFSR halt flag our halt set cleared. A core halted before stays halted.
fn resume(memory: &mut dyn ArmMemoryInterface, halt: &Halt) -> Result<(), ArmError> {
    let Halt::ByUs { dhcsr, dfsr } = halt else {
        return Ok(());
    };
    run(memory, dhcsr.c_debugen(), dhcsr.c_maskints())?;
    clear_dfsr_halted(memory, dfsr)
}

/// Ends the session as the default `DebugCoreStop` does: the core runs, halted before or not, and
/// halting debug is off.
fn release(memory: &mut dyn ArmMemoryInterface, halt: &Halt) -> Result<(), ArmError> {
    run(memory, false, false)?;
    match halt {
        Halt::ByUs { dfsr, .. } => clear_dfsr_halted(memory, dfsr),
        _ => Ok(()),
    }
}

/// Leaves debug state first, then turns halting debug off if `debugen` is false. C_MASKINTS is
/// only changed while halted, before the write that lets the core run.
fn run(memory: &mut dyn ArmMemoryInterface, debugen: bool, maskints: bool) -> Result<(), ArmError> {
    if maskints {
        let mut masked = Dhcsr(0);
        masked.enable_write();
        masked.set_c_debugen(true);
        masked.set_c_halt(true);
        masked.set_c_maskints(true);
        memory.write_word_32(Dhcsr::get_mmio_address(), masked.into())?;
    }
    let mut run = Dhcsr(0);
    run.enable_write();
    run.set_c_debugen(true);
    run.set_c_maskints(maskints);
    memory.write_word_32(Dhcsr::get_mmio_address(), run.into())?;
    if !debugen {
        let mut off = Dhcsr(0);
        off.enable_write();
        memory.write_word_32(Dhcsr::get_mmio_address(), off.into())?;
    }
    Ok(())
}

/// Clears DFSR.HALTED if it was clear before our halt set it.
fn clear_dfsr_halted(memory: &mut dyn ArmMemoryInterface, before: &Dfsr) -> Result<(), ArmError> {
    if before.halted() {
        return Ok(());
    }
    let mut clear = Dfsr(0);
    clear.set_halted(true);
    memory.write_word_32(Dfsr::get_mmio_address(), clear.into())
}

impl ArmDebugSequence for Stm32Armv6 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        let demcr = memory.read_word_32(Demcr::get_mmio_address())?;
        // Only a power-on reset clears DBGMCU CR (RM0444 40.10.2). This read only decides whether
        // to halt; where CR reads 0 without DBGEN, `apply_cr` reads it again with DBGEN on.
        let cr = dbgmcu::Control::read(&mut *memory)?;
        let pending = !(cr.dbg_stop() && cr.dbg_standby());
        let saved = Saved {
            demcr,
            cr: if pending {
                CrState::Pending
            } else {
                CrState::Untouched
            },
            dbgen_before: None,
        };
        // A new attach: whatever an earlier, failed attach left here is stale. Two sessions at once
        // on one `Target` share this state and cannot both restore it.
        if self.saved.lock().unwrap().replace(saved).is_some() {
            tracing::warn!("State of an earlier session on this target is discarded");
        }
        if !pending {
            return Ok(());
        }

        let halt = halt(&mut *memory)?;
        if let Halt::Failed = halt {
            tracing::debug!("Stop and standby debug are set once the core is halted");
            return Ok(());
        }
        let applied = self.apply_cr(&mut *memory);
        let resumed = resume(&mut *memory, &halt);
        applied.and(resumed)
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // The core is halted out of reset here: the moment to set what attach under reset could
        // not, while RCC was held in reset.
        // A reset in the session puts RCC back, so DBGEN is turned on again too.
        let applied = match Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
            true => self.apply_cr(core).and_then(|()| self.dbgen_on_again(core)),
            false => Ok(()),
        };
        let cleared = cortex_m_reset_catch_clear(core);
        applied.and(cleared)
    }

    fn debug_core_stop(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        let saved = self.saved.lock().unwrap().take();
        let Some(saved) = saved else {
            return self.debug_core_stop_default(memory, core_type);
        };
        // As the default DebugCoreStop, but DEMCR goes back to what attach found, not to 0, and
        // DBGMCU CR and DBGEN are put back. The core is halted only if there is something to put
        // back.
        let (enr_address, dbgen) = self.dbgen_register();
        let changed = matches!(saved.cr, CrState::Written { .. })
            || saved.dbgen_before == Some(false)
                && memory.read_word_32(enr_address)? & 1 << dbgen != 0
            || memory.read_word_32(Demcr::get_mmio_address())? != saved.demcr;
        if !changed {
            return release(memory, &Halt::Already);
        }
        let halt = halt(memory)?;
        let restored = match halt {
            Halt::Failed => {
                tracing::warn!("Core did not halt: DBGMCU CR, DBGEN and DEMCR stay as they are");
                Ok(())
            }
            _ => self.restore(memory, saved),
        };
        let resumed = release(memory, &halt);
        restored.and(resumed)
    }
}
