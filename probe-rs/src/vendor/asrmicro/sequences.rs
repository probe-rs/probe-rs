use crate::{
    MemoryMappedRegister, RegisterId,
    architecture::arm::{
        ArmError, armv7m::Dhcsr, core::cortex_m::write_core_reg, memory::ArmMemoryInterface,
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

const ICTR: u64 = 0xE000_E004;
const SYST_CSR: u64 = 0xE000_E010;
const SYST_RVR: u64 = 0xE000_E014;
const SYST_CVR: u64 = 0xE000_E018;
const NVIC_ICER: u64 = 0xE000_E180;
const NVIC_ICPR: u64 = 0xE000_E280;
const SCB_ICSR: u64 = 0xE000_ED04;
const SCB_VTOR: u64 = 0xE000_ED08;
const SCB_SCR: u64 = 0xE000_ED10;
const SCB_SHCSR: u64 = 0xE000_ED24;

// RCC base 0x4000_0000 — RM §8.3.4 RCC_CGR0 (offset 0x00C).
const RCC_CGR0: u64 = 0x4000_000C;
/// RM §8.3.4 bit 21: clock gate for the SYSCFG peripheral.
const RCC_CGR0_SYSCFG_CLK_EN: u32 = 1 << 21;

// SYSCFG base 0x4000_1000 — RM §7.5.
const SYSCFG_CR2: u64 = 0x4000_1008;
const SYSCFG_CR3: u64 = 0x4000_100C;
/// SYSCFG_CR2 bit 10: allow debug while the CPU is in Sleep/Deepsleep.
const SYSCFG_DBG_SLEEP: u32 = 1 << 10;
/// SYSCFG_CR3 bit 1: allow debug while the CPU is in Stop.
const SYSCFG_DBG_STOP: u32 = 1 << 1;
/// SYSCFG_CR3 bit 0: allow debug while the CPU is in Standby.
const SYSCFG_DBG_STANDBY: u32 = 1 << 0;

const REG_SP: RegisterId = RegisterId(13);
const REG_LR: RegisterId = RegisterId(14);
const REG_PC: RegisterId = RegisterId(15);
const REG_XPSR: RegisterId = RegisterId(16);
const REG_MSP: RegisterId = RegisterId(17);
const REG_PSP: RegisterId = RegisterId(18);
// { CONTROL[7:0], FAULTMASK[7:0], BASEPRI[7:0], PRIMASK[7:0] }
const REG_SPECIAL: RegisterId = RegisterId(20);

/// Keep SWD working after the application executes WFI/WFE.
///
/// By default the ASR6601 powers down the debug connection when entering low-power
/// modes (Sleep / Stop / Standby). Once that happens the probe times out and cannot
/// reattach until a power cycle or BOOT0 recovery.
///
/// The chip has three sticky "keep debug on" bits in SYSCFG. We set all three so any
/// low-power mode is debug-safe.
fn enable_debug_during_sleep(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    // SYSCFG_CR2 is on the gated SYSCFG clock; we need to enable it before touching CR2.
    let cgr0 = memory.read_word_32(RCC_CGR0)?;
    if cgr0 & RCC_CGR0_SYSCFG_CLK_EN == 0 {
        memory.write_word_32(RCC_CGR0, cgr0 | RCC_CGR0_SYSCFG_CLK_EN)?;
    }

    // SYSCFG_DBG_SLEEP = 1 → "allowed" to keep a debug connection
    // in Sleep/Deepsleep (covers ordinary WFE/WFI with SLEEPDEEP = 0/1).
    let cr2 = memory.read_word_32(SYSCFG_CR2)?;
    memory.write_word_32(SYSCFG_CR2, cr2 | SYSCFG_DBG_SLEEP)?;

    // SYSCFG_DBG_STOP / SYSCFG_DBG_STANDBY = 1 → keep debug
    // when firmware later enters Stop0–3 or Standby (SLEEPDEEP + PWR lp_mode).
    let cr3 = memory.read_word_32(SYSCFG_CR3)?;
    memory.write_word_32(SYSCFG_CR3, cr3 | SYSCFG_DBG_STOP | SYSCFG_DBG_STANDBY)?;

    Ok(())
}

impl ArmDebugSequence for Asr6601 {
    /// SYSRESETREQ drops the ASR6601 debug connection and does not reliably boot
    /// the application under reset catch. Emulate the architectural reset state
    /// while preserving the debug connection.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
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
        if !RAM_RANGE.contains(&sp) || !FLASH_RANGE.contains(&(pc & !1)) || pc & 1 == 0 {
            tracing::warn!(
                "vector table at {VECTOR_TABLE:#010x} is invalid (SP={sp:#010x}, PC={pc:#010x}), \
                 continuing with reset anyway"
            );
        }

        interface.write_word_32(SYST_CSR, 0)?;
        interface.write_word_32(SYST_RVR, 0)?;
        interface.write_word_32(SYST_CVR, 0)?;

        let interrupt_registers = (interface.read_word_32(ICTR)? & 0x0f) + 1;
        for index in 0..interrupt_registers {
            let offset = u64::from(index) * 4;
            interface.write_word_32(NVIC_ICER + offset, u32::MAX)?;
            interface.write_word_32(NVIC_ICPR + offset, u32::MAX)?;
        }

        interface.write_word_32(SCB_ICSR, (1 << 25) | (1 << 27))?;
        interface.write_word_32(SCB_SCR, 0)?;
        interface.write_word_32(SCB_SHCSR, 0)?;
        interface.write_word_32(SCB_VTOR, VECTOR_TABLE as u32)?;

        write_core_reg(interface, REG_SPECIAL, 0)?;
        write_core_reg(interface, REG_XPSR, 1 << 24)?;
        write_core_reg(interface, REG_PSP, 0)?;
        write_core_reg(interface, REG_MSP, sp)?;
        write_core_reg(interface, REG_SP, sp)?;
        write_core_reg(interface, REG_LR, u32::MAX)?;
        write_core_reg(interface, REG_PC, pc)?;

        enable_debug_during_sleep(interface)?;

        Ok(())
    }
}
