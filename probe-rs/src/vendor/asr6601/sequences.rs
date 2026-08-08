use crate::MemoryMappedRegister;
use crate::architecture::arm::ArmError;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use probe_rs_target::CoreType;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Asr6601 {}

impl Asr6601 {
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

const VECTOR_TABLE: u64 = 0x0800_0000;
const FLASH_RANGE: core::ops::RangeInclusive<u32> = 0x0800_0000..=0x0803_FFFF;
// RAM is 0x20000000..0x20010000; the initial SP is the region end (0x20010000).
const RAM_RANGE: core::ops::RangeInclusive<u32> = 0x2000_0000..=0x2001_0000;

impl ArmDebugSequence for Asr6601 {
    /// On the ASR6601, any SCB/AIRCR access while the ROM bootloader runs stalls the
    /// AHB-AP (DAP NACK). A system reset is therefore replaced with an equivalent
    /// "boot from reset vector" performed entirely through the debug interface:
    ///
    /// 1. Halt the core via DHCSR (proven reliable).
    /// 2. Reload SP/MSP and PC from the flash vector table via DCRSR/DCRDR
    ///    (the same memory-mapped core register access used to run flash algorithms).
    ///
    /// This keeps the invariant other probe-rs code relies on: after `reset_and_halt`
    /// the core is halted at the reset vector.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv7m::Dhcsr;

        eprintln!("[ASR6601-DBG] asr6601::reset_system: halt + vector reload (no AIRCR)");

        // 1. Halt the core, re-asserting C_HALT until S_HALT is observed.
        let start = Instant::now();
        loop {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            dhcsr.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            let read = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
            if read.s_halt() {
                break;
            }

            if start.elapsed() >= Duration::from_millis(500) {
                return Err(ArmError::Timeout);
            }

            std::thread::sleep(Duration::from_millis(5));
        }

        // 2. Emulate "boot from reset vector". Only trust the vector table if it
        //    actually looks like one (SP in RAM, PC in flash with the Thumb bit set).
        let sp = interface.read_word_32(VECTOR_TABLE)?;
        let pc = interface.read_word_32(VECTOR_TABLE + 4)?;
        if RAM_RANGE.contains(&sp) && FLASH_RANGE.contains(&(pc & !1)) && pc & 1 == 1 {
            write_core_reg(interface, 13, sp)?; // SP (MSP in handler mode)
            write_core_reg(interface, 17, sp)?; // MSP
            write_core_reg(interface, 15, pc)?; // PC (bit0 = Thumb)
        } else {
            tracing::warn!(
                "Vector table at {VECTOR_TABLE:#010x} does not look valid (SP={sp:#010x}, PC={pc:#010x}); leaving core halted at current PC"
            );
        }

        Ok(())
    }
}

/// Write a core register through DCRDR/DCRSR (same protocol as probe-rs's
/// `cortex_m::write_core_reg`), polling DHCSR.S_REGRDY for completion.
fn write_core_reg(
    interface: &mut dyn ArmMemoryInterface,
    regsel: u32,
    value: u32,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Dhcsr;

    const DCRDR: u64 = 0xE000_EDF8;
    const DCRSR: u64 = 0xE000_EDF4;

    interface.write_word_32(DCRDR, value)?;
    interface.write_word_32(DCRSR, (regsel & 0x7F) | (1 << 16))?;

    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_regrdy() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ArmError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
