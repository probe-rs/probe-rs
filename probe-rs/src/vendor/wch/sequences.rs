//! WCH RISC-V debug sequence with native WCH-Link flash programming.

use std::any::Any;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use probe_rs_target::NvmRegion;

use crate::Error;
use crate::MemoryInterface;
use crate::architecture::riscv::sequences::RiscvDebugSequence;
use crate::flashing::{DebugFlashSequence, FlashLayout};
use crate::probe::wlink::WchLink;
use crate::probe::wlink::flash::{NativeFlashParams, canonical_flash_address, native_flash_params};
use crate::session::Session;

/// Native programming rewrites flash in windows aligned to this size. It must cover the erase
/// unit of every flash loader.
const WINDOW_ALIGN: u64 = 0x2000;

/// Debug sequence for WCH RISC-V chips.
#[derive(Debug)]
pub struct WchRiscvSequence;

impl WchRiscvSequence {
    /// Create a new WCH RISC-V debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self)
    }
}

impl RiscvDebugSequence for WchRiscvSequence {
    fn debug_flash_sequence(&self, session: &mut Session) -> Option<Arc<dyn DebugFlashSequence>> {
        Some(Arc::new(WchNativeFlashSequence::new(session)?))
    }
}

/// Host-side flash programming through native WCH-Link commands.
///
/// The probe's flash loader erases the flash it programs.
#[derive(Debug)]
struct WchNativeFlashSequence {
    params: NativeFlashParams,
    loader: Vec<u8>,
    code_flash: Range<u64>,
}

impl WchNativeFlashSequence {
    /// Returns `None` unless the probe and the target support native flashing.
    fn new(session: &mut Session) -> Option<Self> {
        let driver: &mut dyn Any = session.driver_mut()?;
        let wlink = driver.downcast_mut::<WchLink>()?;
        if !wlink.has_native_data_endpoints() {
            return None;
        }
        let params = native_flash_params(wlink.chip_family())?;
        let target = session.target();
        let loader = target
            .flash_algorithm_by_name(params.algo_name)?
            .instructions
            .clone();
        let code_flash = target
            .memory_map
            .iter()
            .filter_map(|region| region.as_nvm_region())
            .find(|region| {
                !region.is_alias && region.range.start == u64::from(params.code_flash_base)
            })?
            .range
            .clone();
        Some(Self {
            params,
            loader,
            code_flash,
        })
    }

    /// Maps an address to code flash, which the probe firmware expects.
    fn canonical(&self, address: u64) -> Result<u32, Error> {
        u32::try_from(address)
            .map(|address| canonical_flash_address(&self.params, address))
            .map_err(|_| Error::Other(format!("{address:#x} is outside WCH code flash")))
    }
}

/// Returns the WCH-Link for native commands.
fn wch_link(session: &mut Session) -> Result<&mut WchLink, Error> {
    // The probe firmware uses the program buffer itself.
    session.get_riscv_interface(0)?.clear_abstractauto();
    session
        .driver_mut()
        .and_then(|driver| (driver as &mut dyn Any).downcast_mut::<WchLink>())
        .ok_or_else(|| Error::Other("WCH native flashing requires a WCH-Link".to_string()))
}

/// Prepares the debug module for probe-rs after native commands.
///
/// The probe firmware overwrites the program buffer, and reattaching may let the core run.
fn sync_debug_module(session: &mut Session) -> Result<(), Error> {
    session.get_riscv_interface(0)?.clear_abstractauto();
    session.core(0)?.halt(Duration::from_millis(500))?;
    Ok(())
}

/// Merges the windows aligned to `align` around `pages`, clamped to `bounds`.
fn windows(pages: &[(u64, &[u8])], align: u64, bounds: &Range<u64>) -> Vec<Range<u64>> {
    let mut windows: Vec<Range<u64>> = Vec::new();
    for &(address, data) in pages {
        let start = (address / align * align).max(bounds.start);
        let end = (address + data.len() as u64)
            .next_multiple_of(align)
            .min(bounds.end);
        match windows.last_mut() {
            Some(last) if start <= last.end => last.end = last.end.max(end),
            _ => windows.push(start..end),
        }
    }
    windows
}

impl DebugFlashSequence for WchNativeFlashSequence {
    fn supports_region(&self, region: &NvmRegion) -> bool {
        let Ok(start) = self.canonical(region.range.start) else {
            return false;
        };
        let start = u64::from(start);
        let end = start + (region.range.end - region.range.start);
        self.code_flash.start <= start && end <= self.code_flash.end
    }

    fn prepare_flash(&self, session: &mut Session) -> Result<(), Error> {
        wch_link(session)?.native_reattach()?;
        sync_debug_module(session)
    }

    fn erase_all(&self, session: &mut Session) -> Result<(), Error> {
        let wlink = wch_link(session)?;
        wlink.native_unprotect_flash()?;
        wlink.native_erase_flash()?;
        sync_debug_module(session)
    }

    fn program(&self, _session: &mut Session, _address: u64, _data: &[u8]) -> Result<(), Error> {
        Err(Error::NotImplemented("WCH native page programming"))
    }

    fn program_image(
        &self,
        session: &mut Session,
        regions: &[(&NvmRegion, &FlashLayout)],
    ) -> Result<bool, Error> {
        let mut pages = Vec::new();
        for (_, layout) in regions {
            for page in layout.pages() {
                pages.push((u64::from(self.canonical(page.address())?), page.data()));
            }
        }
        pages.sort_by_key(|&(address, _)| address);

        wch_link(session)?.native_unprotect_flash()?;
        sync_debug_module(session)?;

        // The flash loader can erase more than a page, so rewrite whole windows. Native reads
        // are unreliable, so read the rest of each window through the core.
        let mut images = Vec::new();
        let mut core = session.core(0)?;
        for window in windows(&pages, WINDOW_ALIGN, &self.code_flash) {
            let mut image = vec![0; (window.end - window.start) as usize];
            let mut covered = window.start;
            for &(address, data) in pages.iter().filter(|(address, _)| window.contains(address)) {
                if covered < address {
                    let gap = (covered - window.start) as usize..(address - window.start) as usize;
                    core.read(covered, &mut image[gap])?;
                }
                image[(address - window.start) as usize..][..data.len()].copy_from_slice(data);
                covered = covered.max(address + data.len() as u64);
            }
            if covered < window.end {
                core.read(covered, &mut image[(covered - window.start) as usize..])?;
            }
            images.push((window.start, image));
        }
        drop(core);

        let wlink = wch_link(session)?;
        for (address, image) in images {
            wlink.native_write_flash(address as u32, &image, &self.loader, &self.params)?;
        }
        sync_debug_module(session)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_merge_and_clamp() {
        let bounds = 0x0800_0000..0x0800_7000;
        let pages: [(u64, &[u8]); 4] = [
            (0x0800_0100, &[0; 0x100]),
            (0x0800_1f00, &[0; 0x200]),
            (0x0800_4800, &[0; 0x100]),
            (0x0800_6f00, &[0; 0x100]),
        ];

        assert_eq!(
            windows(&pages, 0x2000, &bounds),
            vec![0x0800_0000..0x0800_7000]
        );
        assert_eq!(
            windows(&pages, 0x1000, &bounds),
            vec![
                0x0800_0000..0x0800_3000,
                0x0800_4000..0x0800_5000,
                0x0800_6000..0x0800_7000,
            ]
        );
    }
}
