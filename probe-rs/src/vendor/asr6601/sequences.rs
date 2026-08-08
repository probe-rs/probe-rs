use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ArmError, armv8m::Dhcsr, core::cortex_m::write_core_reg, memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
};
use probe_rs_target::CoreType;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Asr6601;

impl Asr6601 {
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }
}

const VECTOR_TABLE: u64 = 0x0800_0000;
const FLASH_RANGE: core::ops::RangeInclusive<u32> = 0x0800_0000..=0x0803_FFFF;
// RAM is 0x20000000..0x20010000; the initial SP is the region end (0x20010000).
const RAM_RANGE: core::ops::RangeInclusive<u32> = 0x2000_0000..=0x2001_0000;

impl ArmDebugSequence for Asr6601 {
    /// SYSRESETREQ drops the ASR6601 debug connection. Although the debug port can
    /// be reinitialized afterwards, the core cannot then be halted reliably.
    /// Emulate the state needed by probe-rs by halting the core without resetting
    /// the SoC and loading SP and PC from the main-flash vector table.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Reassert C_HALT until the core enters debug state.
        let start = Instant::now();
        loop {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            dhcsr.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            if Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                break;
            }

            if start.elapsed() >= Duration::from_millis(500) {
                return Err(ArmError::Timeout);
            }

            std::thread::sleep(Duration::from_millis(5));
        }

        let sp = interface.read_word_32(VECTOR_TABLE)?;
        let pc = interface.read_word_32(VECTOR_TABLE + 4)?;
        if RAM_RANGE.contains(&sp) && FLASH_RANGE.contains(&(pc & !1)) && pc & 1 == 1 {
            write_core_reg(interface, RegisterId(13), sp)?; // SP
            write_core_reg(interface, RegisterId(17), sp)?; // MSP
            write_core_reg(interface, RegisterId(15), pc)?; // PC
        } else {
            tracing::warn!(
                "Vector table at {VECTOR_TABLE:#010x} does not look valid (SP={sp:#010x}, PC={pc:#010x}); leaving core halted at current PC"
            );
        }

        Ok(())
    }
}
