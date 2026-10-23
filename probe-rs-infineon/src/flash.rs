//! TC27x program flash (PFLASH) programming.
//!
//! A small loader (`loader/m25_flash_loader.c`) runs on CPU0 from PSPR and executes the flash
//! command sequences; the host drives it through a mailbox in DSPR. The command sequences are
//! the ones of the Infineon iLLD `IfxFlash` driver (Boost Software License 1.0).
//!
//! The TC27x PFLASH has logical sectors of 16 KiB to 256 KiB and 32 byte pages. An erased page
//! does not read as a value: it fails with an uncorrectable ECC error. So the flash is read
//! page by page, and a page that can not be read is erased.
//!
//! A sector the image touches is erased and the image's pages are written, the rest of a
//! partly written page is 0 (the erased value in the target description). Bytes the image does
//! not set stay erased, unless `--restore-unwritten` asked probe-rs to read them back first.
//! A sector that already holds exactly that content is not erased.
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

use crate::device_config;

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
///
/// These are the image data, and the fill bytes of the pages the data touches. probe-rs fills
/// the rest of a sector too: with the old content for `--restore-unwritten`, else with the
/// erased value 0. A page with only fill bytes is written when one of them is not 0; an
/// all-zero fill page stays erased.
pub fn page_patches(layout: &FlashLayout) -> BTreeMap<u32, PagePatch> {
    let data = spans_to_patches(
        layout.pages(),
        layout.data_blocks().iter().map(|b| (b.address(), b.size())),
    );
    let fills = spans_to_patches(
        layout.pages(),
        layout.fills().iter().map(|f| (f.address(), f.size())),
    );
    let mut patches = data;
    for (address, fill) in fills {
        match patches.get_mut(&address) {
            Some(page) => {
                for (byte, fill) in page.iter_mut().zip(fill) {
                    if byte.is_none() {
                        *byte = fill;
                    }
                }
            }
            None if fill.iter().any(|b| b.is_some_and(|b| b != 0)) => {
                patches.insert(address, fill);
            }
            None => {}
        }
    }
    patches
}

fn spans_to_patches(
    pages: &[FlashPage],
    spans: impl Iterator<Item = (u64, u64)>,
) -> BTreeMap<u32, PagePatch> {
    let mut patches = BTreeMap::new();
    for (start, size) in spans {
        let end = start + size;
        let mut address = start;
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

/// The pages of a sector once it is written: the patched pages, with 0 in the bytes a patch
/// does not set, and erased pages elsewhere.
pub fn sector_image(sector: Sector, patches: &BTreeMap<u32, PagePatch>) -> Vec<PageImage> {
    (0..sector.size as usize / PAGE)
        .map(|index| {
            let address = sector.base + (index * PAGE) as u32;
            patches
                .get(&address)
                .map(|patch| patch.map(|byte| byte.unwrap_or(0)))
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
                for (i, chunk) in buffer.as_chunks::<PAGE>().0.iter().enumerate() {
                    let index = (at - base) as usize / PAGE + i;
                    pages[index] = Some(*chunk);
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

/// SCU_CHIPID, which tells the device step and variant apart.
const SCU_CHIPID: u64 = 0xF003_6140;

/// The PFLASH sectors of the connected device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashMap {
    /// Where the map comes from: an AURIX Flasher device file, or the built-in TC27x map.
    pub source: String,
    /// The logical sectors.
    pub sectors: Vec<Sector>,
    /// The bases of the sectors that must not be written.
    pub restricted: BTreeSet<u32>,
}

impl FlashMap {
    /// The built-in TC27x map.
    pub fn builtin() -> Self {
        Self {
            source: "the built-in TC27x map".into(),
            sectors: tc27x_sectors(),
            restricted: BTreeSet::new(),
        }
    }

    /// The map for a device with these IDs: from its AURIX Flasher device file in `dirs` if
    /// there is one, else the built-in TC27x map. Fails for a device this loader cannot program.
    pub fn for_device(
        dirs: &[std::path::PathBuf],
        jtag_id: u32,
        chip_id: Option<u32>,
    ) -> Result<Self, Error> {
        let config = device_config::find_in(dirs, jtag_id, chip_id);
        let is_tc27x = jtag_id & 0x0FFF_FFFF == 0x001D_A083;
        let Some(config) = config else {
            if !is_tc27x {
                return Err(other(format!(
                    "the TC27x flash loader does not support JTAG ID {jtag_id:#010x}"
                )));
            }
            return Ok(Self::builtin());
        };
        let name = &config.device_name;
        if config.flax_file_name != device_config::FLAX_TC2XX {
            return Err(other(format!(
                "{name} uses the flash controller of {}; probe-rs has a flash loader for the \
                 TC27x only",
                config.flax_file_name
            )));
        }
        if !is_tc27x {
            return Err(other(format!(
                "{name} is a TC2xx device, but the probe-rs flash loader is built and tested \
                 for the TC27x only"
            )));
        }
        let pflash = config
            .program_flash()
            .filter(|m| !m.sectors.is_empty())
            .ok_or_else(|| {
                other(format!(
                    "{} has no ProgramFlash sectors",
                    config.file.display()
                ))
            })?;
        let sectors = pflash
            .sectors
            .iter()
            .map(|s| Sector {
                base: s.base,
                size: s.size,
            })
            .collect::<Vec<_>>();
        if sectors != tc27x_sectors() {
            tracing::warn!(
                "The PFLASH map of {name} differs from the built-in TC27x map; using {}",
                config.file.display()
            );
        }
        Ok(Self {
            source: format!("{name} ({})", config.file.display()),
            restricted: pflash
                .sectors
                .iter()
                .filter(|s| s.restricted)
                .map(|s| s.base)
                .collect(),
            sectors,
        })
    }

    fn sector(&self, address: u64) -> Result<Sector, Error> {
        let uncached = to_uncached(address);
        let sector = u32::try_from(uncached)
            .ok()
            .and_then(|a| sector_at(&self.sectors, a))
            .ok_or_else(|| other(format!("{address:#010x} is not in the PFLASH")))?;
        if self.restricted.contains(&sector.base) {
            return Err(other(format!(
                "sector {:#010x} is restricted in {}",
                sector.base, self.source
            )));
        }
        Ok(sector)
    }
}

/// Programs the TC27x PFLASH through the RAM loader.
#[derive(Debug, Default)]
pub struct Tc27xFlash {
    saved: Mutex<Option<SavedState>>,
    /// The pages written in this session, which must read back as data.
    programmed: Mutex<BTreeSet<u32>>,
    /// The sector map of the connected device, found when the loader starts.
    map: Mutex<Option<FlashMap>>,
}

fn other(message: impl Into<String>) -> Error {
    Error::Other(message.into())
}

impl Tc27xFlash {
    /// Checks that the loader can program the device and takes its sector map.
    fn check_device(&self, session: &mut Session) -> Result<(), Error> {
        let jtag_id = session.get_tricore_bus()?.jtag_id()?;
        let chip_id = session.core(0)?.read_word_32(SCU_CHIPID).ok();
        let map = FlashMap::for_device(&device_config::config_dirs(), jtag_id, chip_id)?;
        tracing::info!(
            "PFLASH map: {} ({} sectors; JTAG ID {jtag_id:#010x}, chip ID {})",
            map.source,
            map.sectors.len(),
            chip_id.map_or("?".into(), |c| format!("{c:#010x}"))
        );
        *self.map.lock().unwrap_or_else(|e| e.into_inner()) = Some(map);
        Ok(())
    }

    /// The sector map of the device (the built-in one before the loader started).
    fn flash_map(&self) -> FlashMap {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(FlashMap::builtin)
    }

    fn mailbox_read(session: &mut Session, index: u32) -> Result<u32, Error> {
        session.core(0)?.read_word_32((MAILBOX + 4 * index) as u64)
    }

    fn mailbox_write(session: &mut Session, index: u32, value: u32) -> Result<(), Error> {
        session
            .core(0)?
            .write_word_32((MAILBOX + 4 * index) as u64, value)
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
        tracing::info!(
            "Erasing sector {:#010x} ({} KiB)",
            sector.base,
            sector.size / 1024
        );
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
    fn program_pages(
        &self,
        session: &mut Session,
        base: u32,
        pages: &[PageImage],
    ) -> Result<(), Error> {
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
        self.check_device(session)?;

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
        let sector = self.flash_map().sector(address)?;
        if sector.base as u64 != to_uncached(address) {
            return Err(other(format!(
                "{address:#010x} is not the start of a sector"
            )));
        }
        Self::erase(session, sector)
    }

    fn program(&self, session: &mut Session, address: u64, data: &[u8]) -> Result<(), Error> {
        // Only used when `program_image` is not: write the data as given.
        let base = to_uncached(address) as u32;
        self.flash_map().sector(address)?;
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

        let map = self.flash_map();
        let mut touched = BTreeMap::new();
        for page in patches.keys() {
            let sector = map.sector(*page as u64)?;
            touched.insert(sector.base, sector);
        }

        for sector in touched.into_values() {
            let backup = {
                let mut core = session.core(0)?;
                read_pages(&mut core, sector.base, sector.size as usize)?
            };
            let image = sector_image(sector, &patches);
            if image == backup {
                tracing::info!("Sector {:#010x} already holds the image", sector.base);
                continue;
            }
            Self::erase(session, sector)?;
            self.program_pages(session, sector.base, &image)?;
            tracing::info!(
                "Sector {:#010x}: {} page(s) written",
                sector.base,
                image.iter().filter(|p| p.is_some()).count()
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
        for (index, (page, expected)) in pages.iter().zip(data.as_chunks::<PAGE>().0).enumerate() {
            let address = base + (index * PAGE) as u32;
            let ok = match page {
                Some(actual) => actual == expected,
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
        assert_eq!(
            sectors[0],
            Sector {
                base: 0xA000_0000,
                size: 0x4000
            }
        );
        let last = sectors.last().unwrap();
        assert_eq!(last.base + last.size, 0xA040_0000);
        for pair in sectors.windows(2) {
            assert_eq!(pair[0].base + pair[0].size, pair[1].base);
        }
        // The test sector of the demo.
        assert_eq!(
            sector_at(&sectors, 0xA021_C0B0),
            Some(Sector {
                base: 0xA021_C000,
                size: 0x4000
            })
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
    fn only_patched_pages_are_written() {
        let sector = Sector {
            base: 0xA021_C000,
            size: 0x4000,
        };
        let mut patches = BTreeMap::new();
        let mut patch = [None; PAGE];
        patch[1] = Some(0xAB);
        patches.insert(sector.base + 64, patch);

        let image = sector_image(sector, &patches);
        assert_eq!(image.len(), 512);
        let mut expected = [0; PAGE];
        expected[1] = 0xAB;
        assert_eq!(image[2], Some(expected));
        assert_eq!(image.iter().filter(|p| p.is_some()).count(), 1);
    }

    /// A directory with one AURIX Flasher style device file (made-up content).
    fn device_dir(
        name: &str,
        flax: &str,
        jtag: &str,
        sectors: &[(u32, u32, bool)],
    ) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("probe-rs-flash-map-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sectors = sectors
            .iter()
            .map(|(base, size, restricted)| {
                format!(
                    r#"{{ "sectorStartAddress": "{base:#010X}", "sectorSize": "{size:#X}", "restricted": {restricted} }}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            r#"{{ "deviceName": "{name}", "flaxFileName": "{flax}",
                 "registers": [ {{ "registerName": "JTAGID", "registerValue": ["{jtag}"] }},
                                {{ "registerName": "ChipID", "registerValue": ["0x47427060"] }} ],
                 "memory": [ {{ "memoryName": "ProgramFlash", "memoryStartAddress": "0xA0000000",
                                "memoryEndAddress": "0xA03FFFFF", "logicalSectors": [{sectors}] }} ] }}"#
        );
        std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
        dir
    }

    fn builtin_runs() -> Vec<(u32, u32, bool)> {
        tc27x_sectors()
            .iter()
            .map(|s| (s.base, s.size, false))
            .collect()
    }

    #[test]
    fn the_map_comes_from_the_matching_device_file() {
        let dir = device_dir(
            "TC27x_test",
            "flax_TC2xx.hex",
            "0x401DA083",
            &builtin_runs(),
        );
        let map = FlashMap::for_device(std::slice::from_ref(&dir), 0x401D_A083, Some(0x4742_7060))
            .unwrap();
        assert!(map.source.starts_with("TC27x_test"), "{}", map.source);
        assert_eq!(map.sectors, tc27x_sectors());
        assert_eq!(map.sector(0x8021_C0B0).unwrap().base, 0xA021_C000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restricted_sectors_are_refused() {
        let mut runs = builtin_runs();
        runs[0].2 = true;
        let dir = device_dir("TC27x_restricted", "flax_TC2xx.hex", "0x401DA083", &runs);
        let map = FlashMap::for_device(std::slice::from_ref(&dir), 0x401D_A083, None).unwrap();
        let error = map.sector(0xA000_0100).unwrap_err().to_string();
        assert!(error.contains("restricted"), "{error}");
        assert!(map.sector(0xA000_4000).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn devices_without_a_loader_are_refused() {
        let tc3 = device_dir(
            "TC37x_test",
            "flax_TC37x.hex",
            "0x10207083",
            &[(0xA000_0000, 0x4000, false)],
        );
        let error = FlashMap::for_device(std::slice::from_ref(&tc3), 0x1020_7083, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("flax_TC37x.hex"), "{error}");
        std::fs::remove_dir_all(tc3).unwrap();

        let tc26 = device_dir(
            "TC26x_test",
            "flax_TC2xx.hex",
            "0x201E8083",
            &[(0xA000_0000, 0x4000, false)],
        );
        let error = FlashMap::for_device(std::slice::from_ref(&tc26), 0x201E_8083, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("TC27x only"), "{error}");
        std::fs::remove_dir_all(tc26).unwrap();
    }

    #[test]
    fn without_a_device_file_the_builtin_map_is_used_for_the_tc27x_only() {
        assert_eq!(
            FlashMap::for_device(&[], 0x401D_A083, None).unwrap(),
            FlashMap::builtin()
        );
        assert!(FlashMap::for_device(&[], 0x1020_7083, None).is_err());
    }

    #[test]
    fn the_loader_is_the_reviewed_build() {
        // A change of the loader binary must be deliberate: rebuild with loader/build.ps1 and
        // update this check. SHA-256 9E16F30D38DCF1FCDC3E1C663A67577605999AA5B5B146770D4F2F7380377BC4.
        assert_eq!(LOADER.len(), 946);
    }
}
