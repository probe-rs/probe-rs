//! PPCA acquisition for PSOC C3 x7/x8 devices.

use std::{
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::Chip;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ApV2Address, ArmDebugInterface, ArmError, FullyQualifiedApAddress, core::armv7m::Dhcsr,
        dp::DpAddress,
    },
    config::CoreExt,
};

const PPCA0_AP_BASE: u64 = 0xF000_6000;
const PPCA1_AP_BASE: u64 = 0xF000_8000;
const PERI0_GR4_SL_CTL: u64 = 0x5200_4110;
const PPCA_CNFG_CPU_CTRL: u64 = 0x5300_0110;
const PPCA_CNFG_RST_CTRL: u64 = 0x5300_0114;
const PPCA_MXCM330_CM33_CTL: u64 = 0x5308_0000;
const PPCA_MXCM331_CM33_CTL: u64 = 0x5309_0000;
const PPCA_CPUSS_AP_CTL: u64 = 0x530F_0000;
const CPUSS_AP_CTL: u64 = 0x521C_1000;
const PPCA0_CODE_SRAM: u64 = 0x4301_0000;
const PPCA1_CODE_SRAM: u64 = 0x4303_0000;
const PPCA0_DATA_SRAM: u64 = 0x4302_0000;
const PPCA1_DATA_SRAM: u64 = 0x4304_0000;
const ENDLESS_LOOP_THUMB: u32 = 0xE7FE_E7FE;
const STUB_MSP: u32 = 0x2000_1FFC;
const STUB_RESET_HANDLER: u32 = 0x2000_0009;
const PPCA_AP_CTL_ENABLE: u32 = 0x337;
const CPUSS_AP_CTL_ENABLE: u32 = 0x0000_53F7;

/// PPCA access ports and debugger-owned startup-stub acquisition.
#[derive(Debug)]
pub(super) struct PpcaController {
    aps: Vec<FullyQualifiedApAddress>,
}

impl PpcaController {
    pub(super) fn new(chip: &Chip) -> Self {
        let dp = DpAddress::Default;
        let mut aps = Vec::new();
        for base in [PPCA0_AP_BASE, PPCA1_AP_BASE] {
            let ap = FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::new(base));
            if chip
                .cores
                .iter()
                .any(|core| core.memory_ap() == Some(ap.clone()))
            {
                aps.push(ap);
            }
        }
        Self { aps }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &FullyQualifiedApAddress> {
        self.aps.iter()
    }

    pub(super) fn is_ap(&self, ap: &FullyQualifiedApAddress) -> bool {
        self.aps.contains(ap)
    }

    pub(super) fn core_index(&self, ap: &FullyQualifiedApAddress) -> Option<usize> {
        self.aps.iter().position(|candidate| candidate == ap)
    }

    pub(super) fn acquire(
        &self,
        interface: &mut dyn ArmDebugInterface,
        cm33_ap: &FullyQualifiedApAddress,
        core_index: usize,
        force_full_acquire: bool,
    ) -> Result<(), ArmError> {
        tracing::debug!(
            "PSOC C3 x7/x8: PPCA Core{core_index} acquisition (force_full_acquire={force_full_acquire})"
        );

        let mut cm33 = interface.memory_interface(cm33_ap)?;
        let gr4_val = cm33.read_word_32(PERI0_GR4_SL_CTL)?;
        cm33.write_word_32(PERI0_GR4_SL_CTL, gr4_val | 1)?;

        let cpu_bit = 1u32.checked_shl(core_index as u32).ok_or_else(|| {
            ArmError::Other(format!(
                "PSOC C3 x7/x8: invalid PPCA core index {core_index}"
            ))
        })?;
        let cpuss_ap_ctl = cm33.read_word_32(CPUSS_AP_CTL).unwrap_or(0);
        let rst_ctrl = cm33.read_word_32(PPCA_CNFG_RST_CTRL).unwrap_or(0);
        let cpu_ctrl = cm33.read_word_32(PPCA_CNFG_CPU_CTRL).unwrap_or(0);
        let (code_sram, data_sram) = match core_index {
            0 => (PPCA0_CODE_SRAM, PPCA0_DATA_SRAM),
            1 => (PPCA1_CODE_SRAM, PPCA1_DATA_SRAM),
            _ => {
                return Err(ArmError::Other(format!(
                    "PSOC C3 x7/x8: invalid PPCA core index {core_index}"
                )));
            }
        };

        let stub_valid = cm33.read_word_32(code_sram).unwrap_or(0) == STUB_MSP
            && cm33.read_word_32(code_sram + 4).unwrap_or(0) == STUB_RESET_HANDLER
            && cm33.read_word_32(data_sram + 8).unwrap_or(0) == ENDLESS_LOOP_THUMB;
        let ppca_ap_ctl = cm33.read_word_32(PPCA_CPUSS_AP_CTL).unwrap_or(0);
        let ap_open = (cpuss_ap_ctl & CPUSS_AP_CTL_ENABLE) == CPUSS_AP_CTL_ENABLE
            && (ppca_ap_ctl & PPCA_AP_CTL_ENABLE) == PPCA_AP_CTL_ENABLE;
        let core_enabled = (rst_ctrl & cpu_bit) != 0 && (cpu_ctrl & cpu_bit) != 0;
        let skip = !force_full_acquire && stub_valid && ap_open && core_enabled;

        if skip {
            tracing::debug!("PSOC C3 x7/x8: PPCA Core{core_index} already acquired");
            return Ok(());
        }

        cm33.write_word_32(PPCA_CNFG_CPU_CTRL, cpu_ctrl | cpu_bit)?;
        cm33.write_word_32(PPCA_CNFG_RST_CTRL, rst_ctrl & !cpu_bit)?;
        cm33.write_word_32(code_sram, STUB_MSP)?;
        cm33.write_word_32(code_sram + 4, STUB_RESET_HANDLER)?;
        cm33.write_word_32(data_sram + 8, ENDLESS_LOOP_THUMB)?;
        cm33.write_word_32(PPCA_CNFG_RST_CTRL, rst_ctrl | cpu_bit)?;
        let ctl = match core_index {
            0 => PPCA_MXCM330_CM33_CTL,
            1 => PPCA_MXCM331_CM33_CTL,
            _ => unreachable!(),
        };
        cm33.write_word_32(ctl, 0)?;
        cm33.write_word_32(PPCA_CPUSS_AP_CTL, PPCA_AP_CTL_ENABLE)?;
        cm33.write_word_32(CPUSS_AP_CTL, CPUSS_AP_CTL_ENABLE)?;
        drop(cm33);

        let mut ppca = interface.memory_interface(&self.aps[core_index])?;
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_debugen(true);
        dhcsr.set_c_halt(true);
        dhcsr.enable_write();
        ppca.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        ppca.flush()?;
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if Dhcsr(ppca.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        tracing::warn!("PSOC C3 x7/x8: PPCA Core{core_index} did not confirm halt within 100 ms");
        Ok(())
    }
}
