//! TC27x program flash (PFLASH) programming.
//!
//! A small loader (`loader/m25_flash_loader.c`) runs on CPU0 from PSPR and executes the flash
//! command sequences; the host drives it through a mailbox in DSPR. The command sequences are
//! the ones of the Infineon iLLD `IfxFlash` driver (Boost Software License 1.0).
//!
//! The TC27x PFLASH has logical sectors of 16 KiB to 256 KiB and 32 byte pages. An erased page
//! does not read as a value: it fails with an uncorrectable ECC error. So the flash is read
//! page by page, and a page that can not be read is erased. When a sector is changed, its other
//! pages are kept: the sector is read, erased, and its old and new pages are written back.
//!
//! `prepare_flash` saves the CPU0 registers and the RAM the loader uses, and halts all cores.
//! `finish_flash` restores them and resumes the cores that were running. The application in
//! flash has changed by then, so reset the device after flashing.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
    time::{Duration, Instant},
};

use probe_rs::{
    Error, MemoryInterface, RegisterId, Session,
    architecture::tricore::registers::{CSFR_A0, CSFR_ICR, CSFR_PC, TRICORE_CORE_REGISTERS},
    flashing::{DebugFlashSequence, FlashLayout, FlashPage},
};
use probe_rs_target::NvmRegion;

/// The TC27x loader, built by `loader/build.ps1`.
const LOADER: &[u8] = include_bytes!("../loader/m25_flash_loader_tc27x.bin");

const PSPR: u32 = 0x7010_0000;
const MAILBOX: u32 = 0x7000_F000;
const PAYLOAD: u32 = 0x7000_A000;
const PAYLOAD_SIZE: usize = 0x2000;
const LOADER_SP: u32 = 0x7000_E000;
const MAGIC: u32 = 0x4D32_354D;
/// The RAM the loader uses: payload, stack, data and mailbox in DSPR, code in PSPR.
const LOADER_RAM: [(u32, usize); 2] = [(0x7000_A000, 0x6000), (PSPR, 0x2000)];

const WDTCPU0_CON0: u64 = 0xF003_6100;
const WDTS_CON0: u64 = 0xF003_60F0;

/// The PFLASH program page size.
pub const PAGE: usize = 32;

const MB_MAGIC: u32 = 0;
const MB_CMD: u32 = 1;
const MB_ARG0: u32 = 2;
const MB_ARG1: u32 = 3;
const MB_ARG2: u32 = 4;
const MB_STATUS: u32 = 5;
const MB_DONE: u32 = 6;
const MB_PROGRESS: u32 = 7;

const CMD_ERASE: u32 = 1;
const CMD_PROGRAM: u32 = 2;
const CMD_EXIT: u32 = 3;

const ERASE_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(10);

/// One logical PFLASH sector (non-cached address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sector {
    /// The first address.
    pub base: u32,
    /// The size in bytes.
    pub size: u32,
}

/// The 54 logical sectors of the TC27x PFLASH: per bank of 2 MiB, 8 x 16 KiB, 8 x 32 KiB,
/// 4 x 64 KiB, 3 x 128 KiB and 4 x 256 KiB.
pub fn tc27x_sectors() -> Vec<Sector> {
    const RUNS: [(u32, u32); 5] = [
        (0x4000, 8),
        (0x8000, 8),
        (0x1_0000, 4),
        (0x2_0000, 3),
        (0x4_0000, 4),
    ];
    let mut sectors = Vec::new();
    for bank in [0xA000_0000u32, 0xA020_0000] {
        let mut base = bank;
        for (size, count) in RUNS {
            for _ in 0..count {
                sectors.push(Sector { base, size });
                base += size;
            }
        }
    }
    sectors
}

/// The non-cached alias of a cached PFLASH address (`0x8...` to `0xA...`).
pub fn to_uncached(address: u64) -> u64 {
    if address >> 28 == 0x8 {
        address | 0x2000_0000
    } else {
        address
    }
}

fn sector_at(sectors: &[Sector], address: u32) -> Option<Sector> {
    sectors
        .iter()
        .copied()
        .find(|s| address >= s.base && address - s.base < s.size)
}

/// The content of one page: `None` when the page is erased.
pub type PageImage = Option<[u8; PAGE]>;

/// New data for one page: the bytes to write, `None` where the page keeps its content.
pub type PagePatch = [Option<u8>; PAGE];

/// Collects the bytes `layout` writes, per non-cached page address.
pub fn page_patches(layout: &FlashLayout) -> BTreeMap<u32, PagePatch> {
    let pages = layout.pages();
    let mut patches = BTreeMap::new();
    for block in layout.data_blocks() {
        let end = block.address() + block.size();
        let mut address = block.address();
        while address < end {
            let Some(page) = find_page(pages, address) else {
                break;
            };
            let page_end = page.address() + page.size() as u64;
            let n = (end.min(page_end) - address) as usize;
            let offset = (address - page.address()) as usize;
            for (i, byte) in page.data()[offset..offset + n].iter().enumerate() {
                let target = to_uncached(address + i as u64) as u32;
                let patch: &mut PagePatch = patches
                    .entry(target & !(PAGE as u32 - 1))
                    .or_insert([None; PAGE]);
                patch[target as usize % PAGE] = Some(*byte);
            }
            address += n as u64;
        }
    }
    patches
}

fn find_page(pages: &[FlashPage], address: u64) -> Option<&FlashPage> {
    let index = pages.partition_point(|p| p.address() + p.size() as u64 <= address);
    pages
        .get(index)
        .filter(|p| p.address() <= address && address < p.address() + p.size() as u64)
}

/// The pages of a sector after the patches: old pages are kept, patched bytes replace the old
/// ones, and the rest of a newly written page is 0.
pub fn merge_sector(
    base: u32,
    backup: &[PageImage],
    patches: &BTreeMap<u32, PagePatch>,
) -> Vec<PageImage> {
    backup
        .iter()
        .enumerate()
        .map(|(index, old)| {
            let address = base + (index * PAGE) as u32;
            match patches.get(&address) {
                None => *old,
                Some(patch) => {
                    let mut page = old.unwrap_or([0; PAGE]);
                    for (byte, new) in page.iter_mut().zip(patch) {
                        if let Some(new) = new {
                            *byte = *new;
                        }
                    }
                    Some(page)
                }
            }
        })
        .collect()
}

/// Reads `size` bytes of flash at `address` page by page: a page that can not be read (an
/// erased page) is `None`. Large reads are tried first and split only where they fail.
pub fn read_pages(
    memory: &mut dyn MemoryInterface,
    address: u32,
    size: usize,
) -> Result<Vec<PageImage>, Error> {
    let mut pages = vec![None; size / PAGE];
    fn walk(
        memory: &mut dyn MemoryInterface,
        base: u32,
        address: u32,
        size: usize,
        step: usize,
        pages: &mut [PageImage],
    ) {
        let mut offset = 0;
        while offset < size {
            let n = step.min(size - offset);
            let at = address + offset as u32;
            let mut buffer = vec![0; n];
            if memory.read(at as u64, &mut buffer).is_ok() {
                for (i, chunk) in buffer.chunks_exact(PAGE).enumerate() {
                    let index = (at - base) as usize / PAGE + i;
                    pages[index] = Some(chunk.try_into().unwrap_or([0; PAGE]));
                }
            } else if n > PAGE {
                walk(memory, base, at, n, if n > 512 { 512 } else { PAGE }, pages);
            }
            offset += n;
        }
    }
    walk(memory, address, address, size, 0x1000, &mut pages);
    Ok(pages)
}

#[derive(Debug)]
struct SavedState {
    registers: Vec<(RegisterId, u32)>,
    ram: Vec<(u32, Vec<u8>)>,
    running: Vec<bool>,
}

/// Programs the TC27x PFLASH through the RAM loader.
#[derive(Debug, Default)]
pub struct Tc27xFlash {
    saved: Mutex<Option<SavedState>>,
    /// The pages written in this session, which must read back as data.
    programmed: Mutex<BTreeSet<u32>>,
}

fn other(message: impl Into<String>) -> Error {
    Error::Other(message.into())
}

impl Tc27xFlash {
    fn check_device(session: &mut Session) -> Result<(), Error> {
        let jtag_id = session.get_tricore_bus()?.jtag_id()?;
        if jtag_id & 0x0FFF_FFFF != 0x001D_A083 {
            return Err(other(format!(
                "the TC27x flash loader does not support JTAG ID {jtag_id:#010x}"
            )));
        }
        Ok(())
    }

    fn mailbox_read(session: &mut Session, index: u32) -> Result<u32, Error> {
        session.core(0)?.read_word_32((MAILBOX + 4 * index) as u64)
    }

    fn mailbox_write(session: &mut Session, index: u32, value: u32) -> Result<(), Error> {
        session.core(0)?.write_word_32((MAILBOX + 4 * index) as u64, value)
    }

    fn loader_running(session: &mut Session) -> Result<bool, Error> {
        Ok(Self::mailbox_read(session, MB_MAGIC)? == MAGIC)
    }

    /// Runs one loader command and returns its status (0 = ok).
    fn command(
        session: &mut Session,
        cmd: u32,
        args: [u32; 3],
        timeout: Duration,
    ) -> Result<u32, Error> {
        if !Self::loader_running(session)? {
            return Err(other("the flash loader is not running"));
        }
        Self::mailbox_write(session, MB_DONE, 0)?;
        Self::mailbox_write(session, MB_PROGRESS, 0)?;
        Self::mailbox_write(session, MB_ARG0, args[0])?;
        Self::mailbox_write(session, MB_ARG1, args[1])?;
        Self::mailbox_write(session, MB_ARG2, args[2])?;
        // The loader takes the command and clears it at once: no read back.
        Self::mailbox_write(session, MB_CMD, cmd)?;

        let start = Instant::now();
        while Self::mailbox_read(session, MB_DONE)? != 1 {
            if start.elapsed() > timeout {
                return Err(other(format!("flash loader command {cmd} timed out")));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = Self::mailbox_read(session, MB_STATUS)?;
        let progress = Self::mailbox_read(session, MB_PROGRESS)?;
        tracing::debug!("loader command {cmd} {args:#x?}: status {status:#x}, progress {progress}");
        Ok(status)
    }

    fn erase(session: &mut Session, sector: Sector) -> Result<(), Error> {
        tracing::info!("Erasing sector {:#010x} ({} KiB)", sector.base, sector.size / 1024);
        let status = Self::command(session, CMD_ERASE, [sector.base, 1, 0], ERASE_TIMEOUT)?;
        if status != 0 {
            return Err(other(format!(
                "erasing {:#010x} failed (status {status:#x})",
                sector.base
            )));
        }
        Ok(())
    }

    /// Writes the pages that hold data, in runs of consecutive pages that fit the payload buffer.
    fn program_pages(&self, session: &mut Session, base: u32, pages: &[PageImage]) -> Result<(), Error> {
        let mut index = 0;
        while index < pages.len() {
            if pages[index].is_none() {
                index += 1;
                continue;
            }
            let start = index;
            let mut data = Vec::new();
            while index < pages.len() && data.len() < PAYLOAD_SIZE {
                let Some(page) = pages[index] else { break };
                data.extend_from_slice(&page);
                index += 1;
            }
            let address = base + (start * PAGE) as u32;
            session.core(0)?.write(PAYLOAD as u64, &data)?;
            let status = Self::command(
                session,
                CMD_PROGRAM,
                [address, data.len() as u32, PAYLOAD],
                PROGRAM_TIMEOUT,
            )?;
            if status != 0 {
                return Err(other(format!(
                    "programming {address:#010x} failed (status {status:#x})"
                )));
            }
            let mut programmed = self.programmed.lock().unwrap_or_else(|e| e.into_inner());
            for page in 0..data.len() / PAGE {
                programmed.insert(address + (page * PAGE) as u32);
            }
        }
        Ok(())
    }

    fn start_loader(&self, session: &mut Session) -> Result<(), Error> {
        Self::check_device(session)?;

        let mut running = Vec::new();
        for id in 0..session.list_cores().len() {
            let mut core = session.core(id)?;
            running.push(!core.core_halted()?);
            // The application may reset the device when a core stops answering.
            core.halt(Duration::from_millis(500))?;
        }

        let mut core = session.core(0)?;
        let mut registers = Vec::new();
        for register in TRICORE_CORE_REGISTERS.all_registers() {
            registers.push((register.id, core.read_core_reg::<u32>(register.id)?));
        }
        let mut ram = Vec::new();
        for (address, size) in LOADER_RAM {
            let mut data = vec![0; size];
            core.read(address as u64, &mut data)?;
            ram.push((address, data));
        }

        tracing::info!("Starting the TC27x flash loader ({} bytes)", LOADER.len());
        core.write(PSPR as u64, LOADER)?;
        core.write_word_32((MAILBOX + 4 * MB_MAGIC) as u64, 0)?;
        core.write_core_reg(RegisterId(CSFR_PC), PSPR)?;
        core.write_core_reg(RegisterId(CSFR_A0 + 4 * 10), LOADER_SP)?;
        // No application interrupt may run while a flash bank is busy.
        let icr: u32 = core.read_core_reg(RegisterId(CSFR_ICR))?;
        core.write_core_reg(RegisterId(CSFR_ICR), icr & !(1 << 15))?;
        core.run()?;
        drop(core);

        *self.saved.lock().unwrap_or_else(|e| e.into_inner()) = Some(SavedState {
            registers,
            ram,
            running,
        });

        let start = Instant::now();
        while !Self::loader_running(session)? {
            if start.elapsed() > Duration::from_secs(3) {
                return Err(other("the flash loader did not start"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn stop_loader(&self, session: &mut Session) -> Result<(), Error> {
        let Some(saved) = self.saved.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Ok(());
        };

        if Self::loader_running(session)? {
            Self::command(session, CMD_EXIT, [0; 3], Duration::from_secs(2))?;
        }
        let mut core = session.core(0)?;
        core.halt(Duration::from_millis(500))?;

        // The loader must have set ENDINIT again.
        let cpu0 = core.read_word_32(WDTCPU0_CON0)?;
        let safety = core.read_word_32(WDTS_CON0)?;

        for (address, data) in &saved.ram {
            core.write(*address as u64, data)?;
        }
        for (id, value) in &saved.registers {
            core.write_core_reg(*id, *value)?;
        }
        drop(core);

        for (id, running) in saved.running.iter().enumerate() {
            if *running {
                session.core(id)?.run()?;
            }
        }

        if cpu0 & 1 == 0 || safety & 1 == 0 {
            return Err(other(format!(
                "ENDINIT is still cleared after flashing (WDTCPU0_CON0 {cpu0:#010x}, \
                 WDTS_CON0 {safety:#010x}): reset the device"
            )));
        }
        tracing::info!("Flash loader stopped; reset the device to run the new image");
        Ok(())
    }

    fn sector(address: u64) -> Result<Sector, Error> {
        let uncached = to_uncached(address);
        u32::try_from(uncached)
            .ok()
            .and_then(|a| sector_at(&tc27x_sectors(), a))
            .ok_or_else(|| other(format!("{address:#010x} is not in the TC27x PFLASH")))
    }
}

impl DebugFlashSequence for Tc27xFlash {
    fn prepare_flash(&self, session: &mut Session) -> Result<(), Error> {
        self.programmed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.start_loader(session)
    }

    fn finish_flash(&self, session: &mut Session) -> Result<(), Error> {
        self.stop_loader(session)
    }

    fn erase_all(&self, _session: &mut Session) -> Result<(), Error> {
        Err(Error::NotImplemented("chip erase of the TC27x PFLASH"))
    }

    fn supports_chip_erase(&self) -> bool {
        false
    }

    fn erase_sector(&self, session: &mut Session, address: u64) -> Result<(), Error> {
        let sector = Self::sector(address)?;
        if sector.base as u64 != to_uncached(address) {
            return Err(other(format!("{address:#010x} is not the start of a sector")));
        }
        Self::erase(session, sector)
    }

    fn program(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<(), Error> {
        // Only used when `program_image` is not: write the data as given.
        let base = to_uncached(address) as u32;
        Self::sector(address)?;
        if !base.is_multiple_of(PAGE as u32) {
            return Err(other(format!("{address:#010x} is not page aligned")));
        }
        let pages = data
            .chunks(PAGE)
            .map(|chunk| {
                let mut page = [0; PAGE];
                page[..chunk.len()].copy_from_slice(chunk);
                Some(page)
            })
            .collect::<Vec<_>>();
        self.program_pages(session, base, &pages)
    }

    fn program_image(
        &self,
        session: &mut Session,
        regions: &[(&NvmRegion, &FlashLayout)],
    ) -> Result<bool, Error> {
        let mut patches = BTreeMap::new();
        for (_, layout) in regions {
            patches.extend(page_patches(layout));
        }

        let sectors = tc27x_sectors();
        let mut touched = BTreeMap::new();
        for page in patches.keys() {
            let sector = sector_at(&sectors, *page)
                .ok_or_else(|| other(format!("{page:#010x} is not in the TC27x PFLASH")))?;
            touched.insert(sector.base, sector);
        }

        for sector in touched.into_values() {
            let backup = {
                let mut core = session.core(0)?;
                read_pages(&mut core, sector.base, sector.size as usize)?
            };
            let merged = merge_sector(sector.base, &backup, &patches);
            if merged == backup {
                tracing::info!("Sector {:#010x} already holds the image", sector.base);
                continue;
            }
            Self::erase(session, sector)?;
            self.program_pages(session, sector.base, &merged)?;
            tracing::info!(
                "Sector {:#010x}: {} page(s) written",
                sector.base,
                merged.iter().filter(|p| p.is_some()).count()
            );
        }
        Ok(true)
    }

    fn verify(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<bool, Error> {
        let base = to_uncached(address) as u32;
        let pages = {
            let mut core = session.core(0)?;
            read_pages(&mut core, base, data.len() / PAGE * PAGE)?
        };
        let programmed = self.programmed.lock().unwrap_or_else(|e| e.into_inner());
        for (index, (page, expected)) in pages.iter().zip(data.chunks_exact(PAGE)).enumerate() {
            let address = base + (index * PAGE) as u32;
            let ok = match page {
                Some(actual) => actual.as_slice() == expected,
                // An erased page stands for bytes the image does not set; it must not be one
                // that was just written.
                None => !programmed.contains(&address) && expected.iter().all(|b| *b == 0),
            };
            if !ok {
                tracing::error!("Verify failed at {address:#010x}");
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sector_map_has_54_sectors_and_covers_4_mib() {
        let sectors = tc27x_sectors();
        assert_eq!(sectors.len(), 54);
        assert_eq!(sectors[0], Sector { base: 0xA000_0000, size: 0x4000 });
        let last = sectors.last().unwrap();
        assert_eq!(last.base + last.size, 0xA040_0000);
        for pair in sectors.windows(2) {
            assert_eq!(pair[0].base + pair[0].size, pair[1].base);
        }
        // The test sector of the demo.
        assert_eq!(
            sector_at(&sectors, 0xA021_C0B0),
            Some(Sector { base: 0xA021_C000, size: 0x4000 })
        );
        assert_eq!(sector_at(&sectors, 0xA040_0000), None);
    }

    #[test]
    fn cached_addresses_map_to_the_uncached_alias() {
        assert_eq!(to_uncached(0x8021_C000), 0xA021_C000);
        assert_eq!(to_uncached(0xA021_C000), 0xA021_C000);
        assert_eq!(to_uncached(0x7000_0000), 0x7000_0000);
    }

    #[test]
    fn merging_keeps_old_pages_and_erased_pages() {
        let base = 0xA021_C000;
        let mut backup = vec![None; 4];
        backup[2] = Some([0x55; PAGE]);
        let mut patches = BTreeMap::new();
        let mut patch = [None; PAGE];
        patch[1] = Some(0xAB);
        patches.insert(base, patch);

        let merged = merge_sector(base, &backup, &patches);
        let mut expected = [0; PAGE];
        expected[1] = 0xAB;
        assert_eq!(merged, vec![Some(expected), None, Some([0x55; PAGE]), None]);

        // A patch on an existing page keeps its other bytes.
        let mut patches = BTreeMap::new();
        let mut patch = [None; PAGE];
        patch[0] = Some(0x00);
        patches.insert(base + 64, patch);
        let merged = merge_sector(base, &backup, &patches);
        let mut expected = [0x55; PAGE];
        expected[0] = 0;
        assert_eq!(merged[2], Some(expected));
    }

    #[test]
    fn the_loader_is_the_reviewed_build() {
        // A change of the loader binary must be deliberate: rebuild with loader/build.ps1 and
        // update this check. SHA-256 9E16F30D38DCF1FCDC3E1C663A67577605999AA5B5B146770D4F2F7380377BC4.
        assert_eq!(LOADER.len(), 946);
    }
}
