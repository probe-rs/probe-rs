//! RaspberryPi microcontroller support
use jep106::JEP106Code;
use probe_rs_target::Chip;
use sequences::rp235x::Rp235x;
use sequences::rp235x_riscv::Rp235xRiscv;
use sequences::rp2040::Rp2040;
use std::time::Duration;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ApAddress, ApV2Address, ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress,
        ap::{ApRegister, CSW, DRW, TAR},
        armv8m::{Aircr, Dhcsr},
        dp::DpAddress,
    },
    architecture::riscv::{
        Dmcontrol,
        communication_interface::{
            MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface,
            RiscvCommunicationInterfaceState,
        },
        dtm::mem_ap_dtm::MemApDtm,
    },
    config::{DebugSequence, Registry},
    error::Error,
    memory::MemoryInterface,
    vendor::Vendor,
};

pub mod sequences;

/// Cortex-M / RISC-V mem-AP bases in the RP235x Class 9 ROM.
const RP235X_ARM_MEM_AP: u64 = 0x2000;
const RP235X_ARM_CORE1_MEM_AP: u64 = 0x4000;
const RP235X_RISCV_MEM_AP: u64 = 0xa000;

const CHIPID_RP2040: u32 = 0x0000_2927;
const CHIPID_RP235X: u32 = 0x0000_4927;

/// OTP.ARCHSEL — architecture select, sampled on the next processor reset.
const ARCHSEL_ADDR: u64 = 0x4012_0158;
const ARCHSEL_STATUS_ADDR: u64 = 0x4012_015c;
const ARCHSEL_BOTH_ARM: u32 = 0x0;
const ARCHSEL_BOTH_RISCV: u32 = 0x3;

/// PSM.FRCE_OFF — pulse both processor sockets (RP2350 bits 23/24).
const PSM_FRCE_OFF: u64 = 0x4001_8004;
const PSM_FRCE_OFF_BOTH_PROCS: u32 = (1 << 23) | (1 << 24);

/// Live RP235x processor architecture, inferred from present Mem-APs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rp235xArch {
    Arm,
    Riscv,
}

impl Rp235xArch {
    pub(crate) fn from_chip_name(name: &str) -> Option<Self> {
        if !is_rp235x_chip(name) {
            return None;
        }
        if name.contains("riscv") {
            Some(Self::Riscv)
        } else {
            Some(Self::Arm)
        }
    }

    pub(crate) fn target_name(self) -> &'static str {
        match self {
            Self::Arm => "RP235x",
            Self::Riscv => "RP235x_riscv",
        }
    }

    fn archsel_bits(self) -> u32 {
        match self {
            Self::Arm => ARCHSEL_BOTH_ARM,
            Self::Riscv => ARCHSEL_BOTH_RISCV,
        }
    }
}

pub(crate) fn is_rp235x_chip(name: &str) -> bool {
    name.starts_with("RP235")
}

fn ap_v2_base(ap: &FullyQualifiedApAddress) -> Option<u64> {
    match ap.ap() {
        ApAddress::V2(ApV2Address(Some(base))) => Some(*base),
        _ => None,
    }
}

/// Detect which RP235x architecture is currently connected, from live APv2 presence.
pub(crate) fn detect_rp235x_arch(
    interface: &mut dyn ArmDebugInterface,
) -> Result<Option<Rp235xArch>, Error> {
    let aps = match interface.access_ports(DpAddress::Default) {
        Ok(aps) => aps,
        Err(e) => {
            tracing::debug!("RP235x AP enumerate failed: {e}");
            return Ok(None);
        }
    };

    let has_arm = aps
        .iter()
        .any(|ap| ap_v2_base(ap) == Some(RP235X_ARM_MEM_AP));
    let has_riscv = aps
        .iter()
        .any(|ap| ap_v2_base(ap) == Some(RP235X_RISCV_MEM_AP));

    if has_riscv && !has_arm {
        return Ok(Some(Rp235xArch::Riscv));
    }
    if has_arm {
        return Ok(Some(Rp235xArch::Arm));
    }
    Ok(None)
}

fn force_riscv_program_buffer(interface: &mut RiscvCommunicationInterface) {
    let config = interface.memory_access_config();
    for width in [RiscvBusAccess::A8, RiscvBusAccess::A16, RiscvBusAccess::A32] {
        config.set_default_method(width, MemoryAccessMethod::ProgramBuffer);
    }
}

fn open_riscv_dm(
    interface: &mut dyn ArmDebugInterface,
) -> Result<(MemApDtm<'_>, RiscvCommunicationInterfaceState), Error> {
    let ap = FullyQualifiedApAddress::v2_with_dp(
        DpAddress::Default,
        ApV2Address(Some(RP235X_RISCV_MEM_AP)),
    );
    let memory = interface.memory_interface(&ap).map_err(Error::Arm)?;
    Ok((
        MemApDtm::new(memory),
        RiscvCommunicationInterfaceState::new(),
    ))
}

fn halt_both_riscv(
    riscv: &mut RiscvCommunicationInterface,
    timeout: Duration,
) -> Result<(), Error> {
    riscv.set_enabled_harts(0b11);
    let _ = riscv.select_hart(1).and_then(|_| riscv.halt(timeout));
    riscv.select_hart(0).map_err(Error::Riscv)?;
    riscv.halt(timeout).map_err(Error::Riscv)?;
    Ok(())
}

fn hartreset_both(riscv: &mut RiscvCommunicationInterface) -> Result<(), Error> {
    riscv.set_enabled_harts(0b11);
    riscv.select_hart(0).map_err(Error::Riscv)?;
    let _ = riscv.reset_hart_and_halt(Duration::from_millis(200));
    let _ = riscv
        .select_hart(1)
        .and_then(|_| riscv.reset_hart_and_halt(Duration::from_millis(200)));
    riscv.select_hart(0).map_err(Error::Riscv)?;
    Ok(())
}

/// Pulse `hartreset` on both harts without `haltreq` so sockets resample ARCHSEL.
///
/// OpenOCD: `hasel` + hart window `0b11`, then `dmcontrol = hartreset|dmactive`.
/// `reset_hart_and_halt` holds `haltreq` and waits for RISC-V `allhalted`.
fn pulse_hartreset_for_archsel(riscv: &mut RiscvCommunicationInterface) {
    // OpenOCD: `riscv dm_write 0x10 0x20000001` then `0x00000001` (hartreset|dmactive, no haltreq).
    let mut dmcontrol = Dmcontrol(0);
    dmcontrol.set_dmactive(true);
    dmcontrol.set_haltreq(false);
    dmcontrol.set_ndmreset(false);
    dmcontrol.set_hartreset(true);
    if let Err(e) = riscv.write_dm_register(dmcontrol) {
        tracing::warn!("RP235x hartreset assert failed: {e}");
        return;
    }
    dmcontrol.set_hartreset(false);
    let _ = riscv.write_dm_register(dmcontrol);
}

fn write_archsel_via_arm(interface: &mut dyn ArmDebugInterface, bits: u32) -> Result<u32, Error> {
    let ap = FullyQualifiedApAddress::v2_with_dp(
        DpAddress::Default,
        ApV2Address(Some(RP235X_ARM_MEM_AP)),
    );
    let mut memory = interface.memory_interface(&ap).map_err(Error::Arm)?;
    memory
        .write_word_32(ARCHSEL_ADDR, bits)
        .map_err(Error::Arm)?;
    let readback = memory.read_word_32(ARCHSEL_ADDR).map_err(Error::Arm)?;
    Ok(readback & 0x3)
}

fn write_archsel_via_riscv(interface: &mut dyn ArmDebugInterface, bits: u32) -> Result<u32, Error> {
    let (dtm, mut state) = open_riscv_dm(interface)?;
    let mut riscv = RiscvCommunicationInterface::new(Box::new(dtm), &mut state);
    riscv.enter_debug_mode().map_err(Error::Riscv)?;
    force_riscv_program_buffer(&mut riscv);
    let _ = halt_both_riscv(&mut riscv, Duration::from_millis(100));
    riscv.select_hart(0).map_err(Error::Riscv)?;

    riscv.write_word_32(ARCHSEL_ADDR, bits)?;
    let readback = riscv.read_word_32(ARCHSEL_ADDR)?;
    let status = riscv.read_word_32(ARCHSEL_STATUS_ADDR).unwrap_or(0);
    tracing::info!(
        "RP235x ARCHSEL write {bits:#x}, readback {readback:#x}, ARCHSEL_STATUS {status:#x}"
    );
    Ok(readback & 0x3)
}

fn warm_reset_arm_processors(interface: &mut dyn ArmDebugInterface) -> Result<(), Error> {
    // SYSRESETREQ on BOTH M33 Mem-APs so both sockets resample ARCHSEL.
    // Do not hold PSM.FRCE_OFF across a delay.
    tracing::info!("RP235x Arm warm reset: SYSRESETREQ on core0+core1 Mem-APs");
    for base in [RP235X_ARM_MEM_AP, RP235X_ARM_CORE1_MEM_AP] {
        let ap = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(base)));
        if let Ok(mut memory) = interface.memory_interface(&ap) {
            let mut aircr = Aircr(0);
            aircr.vectkey();
            aircr.set_sysresetreq(true);
            let _ = memory.write_word_32(Aircr::get_mmio_address(), aircr.into());
        }
    }
    Ok(())
}

fn settle_arm_core1(interface: &mut dyn ArmDebugInterface) {
    tracing::info!("RP235x Arm settle: re-halt core0 (no PSM pulse)");
    if try_halt_arm_ap(interface, RP235X_ARM_MEM_AP, true) {
        tracing::info!("RP235x Arm settle: core0 S_HALT");
    } else {
        tracing::warn!("RP235x Arm settle: core0 not S_HALT");
    }
}

fn warm_reset_riscv_processors(interface: &mut dyn ArmDebugInterface) -> Result<(), Error> {
    let (dtm, mut state) = open_riscv_dm(interface)?;
    let mut riscv = RiscvCommunicationInterface::new(Box::new(dtm), &mut state);
    riscv.enter_debug_mode().map_err(Error::Riscv)?;
    force_riscv_program_buffer(&mut riscv);
    let _ = halt_both_riscv(&mut riscv, Duration::from_millis(100));
    tracing::info!("RP235x RISC-V warm reset: PSM.FRCE_OFF both PROCs then hartreset (no haltreq)");
    // Write-0 cannot stick for PROC0 (progbuf hart powers off). Arm catch clears FRCE_OFF.
    let _ = riscv.write_word_32(PSM_FRCE_OFF, PSM_FRCE_OFF_BOTH_PROCS);
    pulse_hartreset_for_archsel(&mut riscv);
    drop(riscv);
    Ok(())
}

/// Bootrom auto-switch needs a full processor socket restart (OpenOCD `reset run`).
fn warm_reset_riscv_processors_with_psm(
    interface: &mut dyn ArmDebugInterface,
) -> Result<(), Error> {
    let (dtm, mut state) = open_riscv_dm(interface)?;
    let mut riscv = RiscvCommunicationInterface::new(Box::new(dtm), &mut state);
    riscv.enter_debug_mode().map_err(Error::Riscv)?;
    force_riscv_program_buffer(&mut riscv);
    let _ = halt_both_riscv(&mut riscv, Duration::from_millis(100));

    tracing::info!("RP235x RISC-V warm reset: PSM.FRCE_OFF both PROCs + dual hartreset");
    let _ = riscv.write_word_32(PSM_FRCE_OFF, PSM_FRCE_OFF_BOTH_PROCS);
    hartreset_both(&mut riscv)?;
    drop(riscv);
    Ok(())
}

/// After programming the other architecture's flash image, align ARCHSEL with
/// that IMAGE_DEF, reset both sockets, allow bootrom to finish, then halt the
/// new cores.
pub(crate) fn rebind_after_bootrom_switch(
    interface: &mut dyn ArmDebugInterface,
    desired: Rp235xArch,
) -> Result<(), Error> {
    let live = detect_rp235x_arch(interface)?;
    let bits = desired.archsel_bits();
    tracing::info!(
        "RP235x boot switch: live {live:?} → {desired:?} (ARCHSEL={bits:#x} + IMAGE_DEF)"
    );

    let written = match live {
        Some(Rp235xArch::Arm) | None => write_archsel_via_arm(interface, bits),
        Some(Rp235xArch::Riscv) => write_archsel_via_riscv(interface, bits),
    }?;
    if written != bits {
        return Err(Error::Other(format!(
            "RP235x boot switch: OTP forbids {} (ARCHSEL readback {written:#x})",
            desired.target_name()
        )));
    }

    match live {
        Some(Rp235xArch::Arm) | None => warm_reset_arm_processors(interface)?,
        Some(Rp235xArch::Riscv) => warm_reset_riscv_processors_with_psm(interface)?,
    }

    std::thread::sleep(Duration::from_millis(500));
    let _ = interface.reinitialize();
    let _ = interface.select_debug_port(DpAddress::Default);

    if catch_switched_cores(interface, desired) {
        let _ = interface.reinitialize();
        if desired == Rp235xArch::Arm {
            settle_arm_core1(interface);
        }
        return Ok(());
    }

    match detect_rp235x_arch(interface)? {
        Some(now) if now == desired => Ok(()),
        Some(now) => Err(Error::Other(format!(
            "RP235x boot switch to {} failed (still {:?}; IMAGE_DEF / OTP?)",
            desired.target_name(),
            now
        ))),
        None => Err(Error::Other(format!(
            "RP235x boot switch to {} failed (no live CPU Mem-AP)",
            desired.target_name()
        ))),
    }
}

/// Switch both RP2350 cores to `desired` via OTP.ARCHSEL + warm processor reset.
///
/// Does **not** use RP-AP rescue (that clears RAM and ARCHSEL).
pub(crate) fn switch_rp235x_architecture(
    interface: &mut dyn ArmDebugInterface,
    desired: Rp235xArch,
) -> Result<(), Error> {
    let live = detect_rp235x_arch(interface)?;
    if live == Some(desired) {
        return Ok(());
    }

    let bits = desired.archsel_bits();
    let written = match live {
        Some(Rp235xArch::Arm) | None => write_archsel_via_arm(interface, bits),
        Some(Rp235xArch::Riscv) => write_archsel_via_riscv(interface, bits),
    }?;

    if written != bits {
        return Err(Error::Other(format!(
            "RP235x OTP forbids switching to {} (ARCHSEL readback {written:#x}, wanted {bits:#x})",
            desired.target_name()
        )));
    }

    match live {
        Some(Rp235xArch::Arm) | None => warm_reset_arm_processors(interface)?,
        Some(Rp235xArch::Riscv) => {
            if let Some(entry) = class9_rom_entry(interface, 0) {
                tracing::info!("RP235x Class9[0] before hartreset {entry:#010x}");
            }
            warm_reset_riscv_processors(interface)?;
        }
    }

    // Halt the new cores immediately. Do not Class-9-walk (`detect_rp235x_arch`)
    // during/after the race — that hangs on a sticky FAULT Mem-AP.
    let _ = interface.select_debug_port(DpAddress::Default);
    tracing::info!("RP235x switch: catching {desired:?} cores after warm reset");
    if catch_switched_cores(interface, desired) {
        let _ = interface.reinitialize();
        std::thread::sleep(Duration::from_millis(50));
        if desired == Rp235xArch::Arm {
            settle_arm_core1(interface);
        }
        return Ok(());
    }

    Err(Error::Other(format!(
        "RP235x architecture switch to {} failed (new cores not PRESENT/S_HALT; IMAGE_DEF race or OTP)",
        desired.target_name()
    )))
}

fn catch_switched_cores(interface: &mut dyn ArmDebugInterface, arch: Rp235xArch) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    let mut attempts = 0u32;
    let mut arm_present = false;
    let mut cleared_psm = false;
    let mut logged_dfsr = false;
    while std::time::Instant::now() < deadline {
        attempts += 1;

        let ok = match arch {
            Rp235xArch::Arm => {
                if !arm_present {
                    if let Some(entry) = class9_rom_entry(interface, 0) {
                        tracing::info!("RP235x catch: Class9[0]={entry:#010x}");
                        arm_present = (entry & 1) == 1;
                    }
                    if !arm_present {
                        continue;
                    }
                    tracing::info!("RP235x catch: AP 0x2000 PRESENT after {attempts} polls");
                }
                // Do not reinitialize() here: DP power-down/up leaves DeviceEn=0.
                match arm_ap_csw_device_en(interface, RP235X_ARM_MEM_AP) {
                    None => continue,
                    Some(false) => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Some(true) => {}
                }
                if !cleared_psm {
                    cleared_psm = true;
                    let ap = FullyQualifiedApAddress::v2_with_dp(
                        DpAddress::Default,
                        ApV2Address(Some(RP235X_ARM_MEM_AP)),
                    );
                    match ap_read_mem32(interface, &ap, PSM_FRCE_OFF as u32) {
                        Ok(frce) => {
                            tracing::info!("RP235x catch: PSM.FRCE_OFF {frce:#010x}");
                            if frce & PSM_FRCE_OFF_BOTH_PROCS != 0 {
                                let _ = ap_write_mem32(
                                    interface,
                                    &ap,
                                    PSM_FRCE_OFF as u32,
                                    frce & !PSM_FRCE_OFF_BOTH_PROCS,
                                );
                                std::thread::sleep(Duration::from_millis(50));
                            }
                        }
                        Err(e) => tracing::info!("RP235x catch: PSM.FRCE_OFF read failed: {e}"),
                    }
                }
                let core0 = try_halt_arm_ap(interface, RP235X_ARM_MEM_AP, true);
                if !logged_dfsr {
                    logged_dfsr = true;
                    let ap = FullyQualifiedApAddress::v2_with_dp(
                        DpAddress::Default,
                        ApV2Address(Some(RP235X_ARM_MEM_AP)),
                    );
                    match ap_read_mem32(interface, &ap, 0xe000_ed30) {
                        Ok(dfsr) => {
                            tracing::info!("RP235x catch: DFSR {dfsr:#010x} HALTED={}", dfsr & 1)
                        }
                        Err(e) => tracing::info!("RP235x catch: DFSR read failed: {e}"),
                    }
                }
                if core0 {
                    let _ = try_halt_arm_ap(interface, RP235X_ARM_CORE1_MEM_AP, false);
                }
                core0
            }
            Rp235xArch::Riscv => try_halt_riscv_dm(interface),
        };

        if ok {
            tracing::info!("RP235x switch: halted {arch:?} after {attempts} attempts");
            return true;
        }
    }
    tracing::warn!("RP235x switch: failed to halt {arch:?} after {attempts} attempts");
    false
}

/// Read Mem-AP CSW (AP register, not DRW). `None` = CSW access failed.
fn arm_ap_csw_device_en(interface: &mut dyn ArmDebugInterface, base: u64) -> Option<bool> {
    let ap = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(base)));
    match interface.read_raw_ap_register(&ap, CSW::ADDRESS) {
        Ok(raw) => {
            let csw = CSW::try_from(raw).ok()?;
            let device_en = csw.DeviceEn();
            tracing::info!(
                "RP235x catch: AP {base:#x} CSW {raw:#010x} DeviceEn={} SDeviceEn={} HNONSEC={} TrInProg={}",
                device_en,
                csw.SDeviceEn(),
                (raw >> 30) & 1,
                csw.TrInProg(),
            );
            Some(device_en)
        }
        Err(e) => {
            tracing::info!("RP235x catch: AP {base:#x} CSW read failed: {e}");
            None
        }
    }
}

fn class9_rom_entry(interface: &mut dyn ArmDebugInterface, entry_off: u64) -> Option<u32> {
    let root = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address::root());
    let mut mem = interface.memory_interface(&root).ok()?;
    let base = mem.base_address().ok()?;
    mem.read_word_32(base + entry_off).ok()
}

fn ap_write_mem32(
    interface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
    addr: u32,
    val: u32,
) -> Result<(), crate::architecture::arm::ArmError> {
    interface.write_raw_ap_register(ap, TAR::ADDRESS, addr)?;
    interface.write_raw_ap_register(ap, DRW::ADDRESS, val)?;
    Ok(())
}

fn ap_read_mem32(
    interface: &mut dyn ArmDebugInterface,
    ap: &FullyQualifiedApAddress,
    addr: u32,
) -> Result<u32, crate::architecture::arm::ArmError> {
    interface.write_raw_ap_register(ap, TAR::ADDRESS, addr)?;
    interface.read_raw_ap_register(ap, DRW::ADDRESS)
}

fn try_halt_arm_ap(interface: &mut dyn ArmDebugInterface, base: u64, log: bool) -> bool {
    if log {
        tracing::info!("RP235x catch: try_halt AP {base:#x} (raw TAR/DRW, no CSW rewrite)");
    }
    let ap = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(base)));
    let dhcsr_addr = Dhcsr::get_mmio_address() as u32;

    let before = match ap_read_mem32(interface, &ap, dhcsr_addr) {
        Ok(value) => Dhcsr(value),
        Err(e) => {
            if log {
                tracing::info!("RP235x catch: AP {base:#x} DHCSR before-read failed: {e}");
            }
            let _ = arm_ap_csw_device_en(interface, base);
            return false;
        }
    };
    if log {
        tracing::info!(
            "RP235x catch: AP {base:#x} DHCSR before halt {:#010x} S_SLEEP={} S_HALT={}",
            u32::from(before),
            before.s_sleep(),
            before.s_halt()
        );
    }
    if before.s_halt() {
        return true;
    }
    // C_HALT DRW while S_SLEEP stalls AHB and drops DeviceEn.
    if before.s_sleep() {
        if log {
            tracing::info!("RP235x catch: AP {base:#x} S_SLEEP; skip C_HALT");
        }
        std::thread::sleep(Duration::from_millis(10));
        return false;
    }

    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(true);
    dhcsr.enable_write();
    if let Err(e) = ap_write_mem32(interface, &ap, dhcsr_addr, dhcsr.into()) {
        if log {
            tracing::info!("RP235x catch: AP {base:#x} C_DEBUGEN|C_HALT write failed: {e}");
        }
        let _ = arm_ap_csw_device_en(interface, base);
        return false;
    }

    match ap_read_mem32(interface, &ap, dhcsr_addr) {
        Ok(value) => {
            let dhcsr = Dhcsr(value);
            if log {
                tracing::info!(
                    "RP235x catch: AP {base:#x} DHCSR after halt {value:#010x} S_HALT={}",
                    dhcsr.s_halt()
                );
            }
            dhcsr.s_halt()
        }
        Err(e) => {
            if log {
                tracing::info!("RP235x catch: AP {base:#x} DHCSR read failed: {e}");
            }
            let _ = arm_ap_csw_device_en(interface, base);
            false
        }
    }
}

fn try_halt_riscv_dm(interface: &mut dyn ArmDebugInterface) -> bool {
    let Ok((dtm, mut state)) = open_riscv_dm(interface) else {
        return false;
    };
    let mut riscv = RiscvCommunicationInterface::new(Box::new(dtm), &mut state);
    if riscv.enter_debug_mode().is_err() {
        return false;
    }
    force_riscv_program_buffer(&mut riscv);
    halt_both_riscv(&mut riscv, Duration::from_millis(20)).is_ok()
}

/// Raspberry Pi
#[derive(docsplay::Display)]
pub struct RaspberryPi;

impl Vendor for RaspberryPi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("RP2040") {
            DebugSequence::Arm(Rp2040::create())
        } else if chip.name.starts_with("RP235") && chip.name.contains("riscv") {
            DebugSequence::Riscv(Rp235xRiscv::create())
        } else if chip.name.starts_with("RP235") {
            DebugSequence::Arm(Rp235x::create())
        } else {
            return None;
        };
        Some(sequence)
    }

    fn try_detect_arm_chip(
        &self,
        _registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        const JEP_ARM: JEP106Code = JEP106Code { id: 0x3b, cc: 0x4 };

        // Check for RP2040. We can immediately rule out RP2040 existing if we aren't probing via multidrop.
        if let Some(DpAddress::Multidrop(dp)) = interface.current_debug_port() {
            let ap = FullyQualifiedApAddress::v1_with_dp(DpAddress::Multidrop(dp), 0);
            // Read SYSINFO.CHIP_ID and compare against RP2040 chip_id
            if let Ok(mut memory) = interface.memory_interface(&ap)
                && let Ok(chip_id) = memory.read_word_32(0x4000_0000)
                && (chip_id & 0x0fff_ffff) == CHIPID_RP2040
            {
                return Ok(Some("RP2040".to_string()));
            }
        }

        // RP235x identity arrives two ways:
        // - Arm boot: Cortex-M ROM behind Mem-AP 0x2000 (ARM JEP, PART 1225)
        // - RISC-V boot: DPv3 root Class 9 ROM (Raspberry Pi designer; unused Arm APs
        //   are non-PRESENT, so the M33 ROM is not visible)
        let designer = chip_info.manufacturer.get();
        let is_arm_m33_rom = chip_info.manufacturer == JEP_ARM && chip_info.part == 1225;
        let is_rp235x_class9 = designer == Some("Raspberry Pi Trading Ltd");
        if !is_arm_m33_rom && !is_rp235x_class9 {
            return Ok(None);
        }

        match detect_rp235x_arch(interface)? {
            Some(Rp235xArch::Riscv) => return Ok(Some("RP235x_riscv".to_string())),
            Some(Rp235xArch::Arm) => {
                let ap = FullyQualifiedApAddress::v2_with_dp(
                    DpAddress::Default,
                    ApV2Address(Some(RP235X_ARM_MEM_AP)),
                );
                if let Ok(mut memory) = interface.memory_interface(&ap)
                    && let Ok(chip_id) = memory.read_word_32(0x4000_0000)
                    && (chip_id & 0x0fff_ffff) != CHIPID_RP235X
                {
                    tracing::debug!(
                        "RP235x Arm AP present but CHIP_ID {chip_id:#010x} != {CHIPID_RP235X:#010x}"
                    );
                }
                return Ok(Some("RP235x".to_string()));
            }
            None => {}
        }

        Ok(None)
    }
}
