//! Debug and host-side flash sequences for GigaDevice GD32H7 targets.

use std::sync::Arc;
use std::{
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::{CoreType, MemoryRegion};

use crate::{
    Error, MemoryMappedRegister, Session, Target,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress, Pins,
        core::{armv7m::Demcr, cortex_m::Dhcsr},
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, DebugFlashSequence},
    },
    config::CoreExt,
};

const FMC_BASE: u64 = 0x5200_2000;
const FMC_KEY: u64 = FMC_BASE + 0x04;
const FMC_CTL: u64 = FMC_BASE + 0x0c;
const FMC_STAT: u64 = FMC_BASE + 0x10;
const FMC_CCR: u64 = FMC_BASE + 0x14;
const FMC_OBSTAT1_EFT: u64 = FMC_BASE + 0x50;

const FMC_KEY1: u32 = 0x4567_0123;
const FMC_KEY2: u32 = 0xCDEF_89AB;

const FMC_CTL_LOCK: u32 = 1 << 0;
const FMC_CTL_PG: u32 = 1 << 1;
const FMC_CTL_SER: u32 = 1 << 2;
const FMC_CTL_BER: u32 = 1 << 3;
const FMC_CTL_START: u32 = 1 << 7;

const FMC_STAT_BUSY: u32 = 1 << 0;
const FMC_STAT_ENDF: u32 = 1 << 16;
const FMC_STAT_WPERR: u32 = 1 << 17;
const FMC_STAT_PGSERR: u32 = 1 << 18;
const FMC_STAT_RDPERR: u32 = 1 << 23;
const FMC_STAT_RDSERR: u32 = 1 << 24;
const FMC_STAT_OBMERR: u32 = 1 << 30;
const FMC_STAT_ERROR: u32 =
    FMC_STAT_WPERR | FMC_STAT_PGSERR | FMC_STAT_RDPERR | FMC_STAT_RDSERR | FMC_STAT_OBMERR;
const FMC_STAT_CLEARABLE: u32 = FMC_STAT_ENDF | FMC_STAT_ERROR;

const FLASH_START: u64 = 0x0800_0000;
const FLASH_END: u64 = 0x0810_0000;
const FLASH_SECTOR_SIZE: u64 = 0x1000;
const FLASH_PROGRAM_SIZE: usize = 32;
const FMC_TIMEOUT: Duration = Duration::from_secs(30);

/// Effective FlexRAM layout selected by the device's Option Byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlexRamLayout {
    itcm_size: u64,
    dtcm_size: u64,
    axi_size: u64,
}

/// GD32H7 debug sequence.
#[derive(Debug)]
pub struct Gd32H7Sequence;

impl Gd32H7Sequence {
    /// Create the GD32H7 debug sequence.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self)
    }

    fn update_memory_map(
        memory_map: &mut [MemoryRegion],
        layout: FlexRamLayout,
    ) -> Result<(), ArmError> {
        let mut found_itcm = false;
        let mut found_dtcm = false;
        let mut found_axi = false;

        for region in memory_map {
            let MemoryRegion::Ram(region) = region else {
                continue;
            };

            match (region.name.as_deref(), region.range.start) {
                (Some("ITCMRAM"), 0x0000_0000) => {
                    region.range.end = region.range.start + layout.itcm_size;
                    found_itcm = true;
                }
                (Some("DTCMRAM"), 0x2000_0000) => {
                    region.range.end = region.range.start + layout.dtcm_size;
                    found_dtcm = true;
                }
                (Some("AXISRAM"), 0x2400_0000) => {
                    region.range.end = region.range.start + layout.axi_size;
                    found_axi = true;
                }
                _ => {}
            }
        }

        if found_itcm && found_dtcm && found_axi {
            Ok(())
        } else {
            Err(ArmError::Other(
                "GD32H7 target is missing an ITCMRAM, DTCMRAM, or AXISRAM region".into(),
            ))
        }
    }
}

impl ArmDebugSequence for Gd32H7Sequence {
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if !matches!(
            core_type,
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m
        ) {
            return Err(ArmError::ArchitectureRequired(&["Cortex-M"]));
        }

        let mut dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);
        if !dhcsr.c_debugen() {
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        }

        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(true);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        // Clear DHCSR sticky status, matching the generic Cortex-M reset catch.
        let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        let can_read_pins = probe.swj_pins(n_reset, n_reset, 0)? != 0xffff_ffff;

        if can_read_pins {
            let start = Instant::now();

            loop {
                if Pins(probe.swj_pins(n_reset, n_reset, 0)? as u8).nreset() {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
                thread::sleep(Duration::from_millis(100));
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        // OpenOCD uses an adapter srst delay of 100 ms for GD32H7. Allow the
        // system power domain and debug port to stabilize after reset release
        // before continuing with debug_core_start.
        thread::sleep(Duration::from_millis(100));

        // GD32H7 requires the SWD debug port to be reinitialized after NRST is
        // released, equivalent to OpenOCD's dap_dp_init_or_reconnect.
        probe.reinitialize()?;
        Ok(())
    }

    fn on_connect(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        target: &mut Target,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        let eft = memory.read_word_32(FMC_OBSTAT1_EFT)?;
        let layout = decode_flexram_layout(eft).map_err(ArmError::Other)?;

        tracing::info!(
            eft = format_args!("0x{eft:08x}"),
            itcm = layout.itcm_size,
            dtcm = layout.dtcm_size,
            axi = layout.axi_size,
            "GD32H7 effective FlexRAM layout"
        );

        Self::update_memory_map(&mut target.memory_map, layout)
    }

    fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        Some(Arc::new(Gd32H7FlashSequence))
    }
}

/// Decode the ITCM/DTCM allocation fields in FMC_OBSTAT1_EFT.
fn decode_flexram_layout(eft: u32) -> Result<FlexRamLayout, String> {
    fn decode_capacity(code: u32, name: &str) -> Result<u64, String> {
        match code {
            0 => Ok(0),
            7 => Ok(64 * 1024),
            8 => Ok(128 * 1024),
            9 => Ok(256 * 1024),
            10 => Ok(512 * 1024),
            _ => Err(format!("invalid GD32H7 {name} allocation code {code}")),
        }
    }

    let itcm_size = decode_capacity(eft & 0xf, "ITCM")?;
    let dtcm_size = decode_capacity((eft >> 4) & 0xf, "DTCM")?;

    if itcm_size + dtcm_size > 512 * 1024 {
        return Err(format!(
            "invalid GD32H7 FlexRAM allocation: ITCM={} KiB, DTCM={} KiB",
            itcm_size / 1024,
            dtcm_size / 1024
        ));
    }

    Ok(FlexRamLayout {
        itcm_size,
        dtcm_size,
        axi_size: 1024 * 1024 - itcm_size - dtcm_size,
    })
}

/// Host-side FMC flash operations for the GD32H737 internal flash.
#[derive(Debug)]
struct Gd32H7FlashSequence;

impl Gd32H7FlashSequence {
    fn memory<'session>(
        session: &'session mut Session,
    ) -> Result<Box<dyn ArmMemoryInterface + 'session>, Error> {
        let ap = session
            .target()
            .default_core()
            .memory_ap()
            .ok_or_else(|| Error::Other("GD32H7 target has no memory AP".into()))?
            .clone();
        let interface = session.get_arm_interface()?;
        Ok(interface.memory_interface(&ap)?)
    }

    fn clear_status(memory: &mut dyn ArmMemoryInterface) -> Result<(), Error> {
        let status = memory.read_word_32(FMC_STAT)?;
        let flags = status & FMC_STAT_CLEARABLE;
        if flags != 0 {
            memory.write_word_32(FMC_CCR, flags)?;
            memory.flush()?;
        }
        Ok(())
    }

    fn unlock(memory: &mut dyn ArmMemoryInterface) -> Result<(), Error> {
        let ctl = memory.read_word_32(FMC_CTL)?;
        if ctl & FMC_CTL_LOCK != 0 {
            memory.write_word_32(FMC_KEY, FMC_KEY1)?;
            memory.write_word_32(FMC_KEY, FMC_KEY2)?;
            memory.flush()?;
        }

        let ctl = memory.read_word_32(FMC_CTL)?;
        if ctl & FMC_CTL_LOCK != 0 {
            return Err(Error::Other("GD32H7 FMC registers remain locked".into()));
        }

        Ok(())
    }

    fn lock(memory: &mut dyn ArmMemoryInterface) -> Result<(), Error> {
        memory.write_word_32(FMC_CTL, FMC_CTL_LOCK)?;
        memory.flush()?;
        Ok(())
    }

    fn wait_for_operation(memory: &mut dyn ArmMemoryInterface) -> Result<(), Error> {
        let deadline = Instant::now() + FMC_TIMEOUT;
        loop {
            let status = memory.read_word_32(FMC_STAT)?;
            if status & FMC_STAT_BUSY == 0 {
                let flags = status & FMC_STAT_CLEARABLE;
                if flags != 0 {
                    memory.write_word_32(FMC_CCR, flags)?;
                    memory.flush()?;
                }
                if status & FMC_STAT_ERROR != 0 {
                    return Err(Error::Other(format!(
                        "GD32H7 FMC operation failed, status=0x{status:08x}"
                    )));
                }
                return Ok(());
            }

            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }

            thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_operation(memory: &mut dyn ArmMemoryInterface) -> Result<(), Error> {
        let result = Self::wait_for_operation(memory);
        let clear_command = memory
            .write_word_32(FMC_CTL, 0)
            .and_then(|_| memory.flush());

        match (result, clear_command) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error.into()),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn check_sector_address(address: u64) -> Result<(), Error> {
        if !(FLASH_START..FLASH_END).contains(&address)
            || !(address - FLASH_START).is_multiple_of(FLASH_SECTOR_SIZE)
        {
            return Err(Error::Other(format!(
                "invalid GD32H7 flash sector address 0x{address:08x}"
            )));
        }
        Ok(())
    }

    fn check_page(address: u64, data: &[u8]) -> Result<(), Error> {
        if !(FLASH_START..FLASH_END).contains(&address)
            || !(address - FLASH_START).is_multiple_of(FLASH_PROGRAM_SIZE as u64)
            || data.len() != FLASH_PROGRAM_SIZE
        {
            return Err(Error::Other(format!(
                "GD32H7 flash programming requires a 32-byte aligned full page at 0x{address:08x}"
            )));
        }
        Ok(())
    }
}

impl DebugFlashSequence for Gd32H7FlashSequence {
    fn prepare_flash(&self, session: &mut Session) -> Result<(), Error> {
        {
            let mut core = session.core(0)?;
            if !core.core_halted()? {
                core.halt(Duration::from_millis(500))?;
            }
        }

        let mut memory = Self::memory(session)?;
        if let Err(error) = Self::unlock(&mut *memory) {
            let _ = Self::lock(&mut *memory);
            return Err(error);
        }
        if let Err(error) = Self::clear_status(&mut *memory) {
            let _ = Self::lock(&mut *memory);
            return Err(error);
        }
        Ok(())
    }

    fn erase_all(&self, session: &mut Session) -> Result<(), Error> {
        let mut memory = Self::memory(session)?;
        memory.write_word_32(FMC_CTL, FMC_CTL_BER)?;
        memory.write_word_32(FMC_CTL, FMC_CTL_BER | FMC_CTL_START)?;
        memory.flush()?;
        Self::finish_operation(&mut *memory)
    }

    fn erase_sector(&self, session: &mut Session, address: u64) -> Result<(), Error> {
        Self::check_sector_address(address)?;

        let mut memory = Self::memory(session)?;
        memory.write_word_32(FMC_CTL, FMC_CTL_SER)?;
        memory.write_word_32(FMC_CCR, address as u32)?;
        memory.write_word_32(FMC_CTL, FMC_CTL_SER | FMC_CTL_START)?;
        memory.flush()?;
        Self::finish_operation(&mut *memory)
    }

    fn program(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<(), Error> {
        Self::check_page(address, data)?;

        let mut memory = Self::memory(session)?;
        memory.write_word_32(FMC_CTL, FMC_CTL_PG)?;
        for (index, word) in data.as_chunks::<4>().0.iter().enumerate() {
            let value = u32::from_le_bytes(*word);
            memory.write_word_32(address + (index * 4) as u64, value)?;
        }
        memory.flush()?;
        Self::finish_operation(&mut *memory)
    }

    fn verify(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<bool, Error> {
        Self::check_page(address, data)?;

        let mut memory = Self::memory(session)?;
        let mut actual = vec![0u8; data.len()];
        memory.read(address, &mut actual)?;
        Ok(actual == data)
    }

    fn finish_flash(&self, session: &mut Session) -> Result<(), Error> {
        let mut memory = Self::memory(session)?;
        Self::lock(&mut *memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_known_flexram_layouts() {
        assert_eq!(
            decode_flexram_layout(0x77).unwrap(),
            FlexRamLayout {
                itcm_size: 64 * 1024,
                dtcm_size: 64 * 1024,
                axi_size: 896 * 1024,
            }
        );
        assert_eq!(
            decode_flexram_layout(0x78).unwrap(),
            FlexRamLayout {
                itcm_size: 128 * 1024,
                dtcm_size: 64 * 1024,
                axi_size: 832 * 1024,
            }
        );
        assert_eq!(
            decode_flexram_layout(0x87).unwrap(),
            FlexRamLayout {
                itcm_size: 64 * 1024,
                dtcm_size: 128 * 1024,
                axi_size: 832 * 1024,
            }
        );
    }

    #[test]
    fn accepts_zero_tcm_and_rejects_reserved_or_oversized_allocations() {
        assert_eq!(
            decode_flexram_layout(0).unwrap(),
            FlexRamLayout {
                itcm_size: 0,
                dtcm_size: 0,
                axi_size: 1024 * 1024,
            }
        );
        assert!(decode_flexram_layout(0x17).is_err());
        assert!(decode_flexram_layout(0xAA).is_err());
    }

    #[test]
    fn applies_only_flexram_regions() {
        let mut map = vec![
            MemoryRegion::Ram(probe_rs_target::RamRegion {
                name: Some("ITCMRAM".into()),
                range: 0..1,
                cores: vec!["main".into()],
                is_alias: false,
                access: None,
            }),
            MemoryRegion::Ram(probe_rs_target::RamRegion {
                name: Some("DTCMRAM".into()),
                range: 0x2000_0000..0x2000_0001,
                cores: vec!["main".into()],
                is_alias: false,
                access: None,
            }),
            MemoryRegion::Ram(probe_rs_target::RamRegion {
                name: Some("AXISRAM".into()),
                range: 0x2400_0000..0x2400_0001,
                cores: vec!["main".into()],
                is_alias: false,
                access: None,
            }),
            MemoryRegion::Ram(probe_rs_target::RamRegion {
                name: Some("SRAM0".into()),
                range: 0x3000_0000..0x3000_4000,
                cores: vec!["main".into()],
                is_alias: false,
                access: None,
            }),
        ];

        Gd32H7Sequence::update_memory_map(
            &mut map,
            FlexRamLayout {
                itcm_size: 128 * 1024,
                dtcm_size: 64 * 1024,
                axi_size: 832 * 1024,
            },
        )
        .unwrap();

        assert_eq!(map[0].address_range(), 0..128 * 1024);
        assert_eq!(map[1].address_range(), 0x2000_0000..0x2000_0000 + 64 * 1024);
        assert_eq!(
            map[2].address_range(),
            0x2400_0000..0x2400_0000 + 832 * 1024
        );
        assert_eq!(map[3].address_range(), 0x3000_0000..0x3000_4000);
    }
}
