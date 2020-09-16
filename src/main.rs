mod logger;
mod pc2frames;

use core::{
    cmp,
    convert::TryInto,
    mem,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{
    collections::{btree_map, BTreeMap, HashSet},
    fs,
    io::{self, Write as _},
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail, Context};
use arrayref::array_ref;
use colored::Colorize as _;
use gimli::{
    read::{CfaRule, DebugFrame, UnwindSection},
    BaseAddresses, EndianSlice, LittleEndian, RegisterRule, UninitializedUnwindContext,
};
use object::{
    read::{File as ElfFile, Object as _, ObjectSection as _},
    SymbolSection,
};
use probe_rs::config::{registry, MemoryRegion, RamRegion};
use probe_rs::{
    flashing::{self, Format},
    Core, CoreRegisterAddress, DebugProbeInfo, DebugProbeSelector, MemoryInterface, Probe, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use structopt::StructOpt;

const TIMEOUT: Duration = Duration::from_secs(1);
const STACK_CANARY: u8 = 0xAA;
const THUMB_BIT: u32 = 1;

fn main() -> Result<(), anyhow::Error> {
    notmain().map(|code| process::exit(code))
}

/// A Cargo runner for microcontrollers.
#[derive(StructOpt)]
#[structopt(name = "probe-run")]
struct Opts {
    /// List supported chips and exit.
    #[structopt(long)]
    list_chips: bool,

    /// Lists all the connected probes and exit.
    #[structopt(long)]
    list_probes: bool,

    /// Enable defmt decoding.
    #[cfg(feature = "defmt")]
    #[structopt(long, conflicts_with = "no_flash")]
    defmt: bool,

    /// The chip to program.
    #[structopt(long, required_unless_one(&["list-chips", "list-probes"]), env = "PROBE_RUN_CHIP")]
    chip: Option<String>,

    /// The probe to use (eg. VID:PID or VID:PID:Serial).
    #[structopt(long, env = "PROBE_RUN_PROBE")]
    probe: Option<String>,

    /// Path to an ELF firmware file.
    #[structopt(name = "ELF", parse(from_os_str), required_unless_one(&["list-chips", "list-probes"]))]
    elf: Option<PathBuf>,

    /// Skip writing the application binary to flash.
    #[structopt(long, conflicts_with = "defmt")]
    no_flash: bool,

    /// Enable more verbose logging.
    #[structopt(short, long)]
    verbose: bool,
}

fn notmain() -> Result<i32, anyhow::Error> {
    let opts: Opts = Opts::from_args();
    logger::init(opts.verbose);

    if opts.list_probes {
        return print_probes();
    }

    if opts.list_chips {
        return print_chips();
    }

    let elf_path = opts.elf.as_deref().unwrap();
    let chip = opts.chip.as_deref().unwrap();
    let bytes = fs::read(elf_path)?;
    let elf = ElfFile::parse(&bytes)?;

    let target = probe_rs::config::registry::get_target_by_name(chip)?;

    let mut ram_region = None;
    for region in &target.memory_map {
        match region {
            MemoryRegion::Ram(ram) => {
                if let Some(old) = &ram_region {
                    log::debug!("multiple RAM regions found ({:?} and {:?}), stack canary will not be available", old, ram);
                } else {
                    ram_region = Some(ram.clone());
                }
            }
            _ => {}
        }
    }

    if let Some(ram) = &ram_region {
        log::debug!(
            "RAM region: 0x{:08X}-0x{:08X}",
            ram.range.start,
            ram.range.end - 1
        );
    }

    #[cfg(feature = "defmt")]
    let (table, locs) = {
        let table = defmt_elf2table::parse(&bytes)?;

        if table.is_none() && opts.defmt {
            bail!("`.defmt` section not found")
        } else if table.is_some() && !opts.defmt {
            log::warn!("application may be using `defmt` but `--defmt` flag was not used");
        }

        let locs = if opts.defmt {
            let table = table.as_ref().unwrap();
            let locs = defmt_elf2table::get_locations(&bytes, table)?;

            if !table.is_empty() && locs.is_empty() {
                log::warn!("insufficient DWARF info; compile your program with `debug = 2` to enable location info");
                None
            } else {
                if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
                    Some(locs)
                } else {
                    log::warn!(
                        "(BUG) location info is incomplete; it will be omitted from the output"
                    );
                    None
                }
            }
        } else {
            None
        };

        (table, locs)
    };

    // sections used in cortex-m-rt
    // NOTE we won't load `.uninit` so it is not included here
    // NOTE we don't load `.bss` because the app (cortex-m-rt) will zero it
    let candidates = [".vector_table", ".text", ".rodata", ".data"];

    let mut highest_ram_addr_in_use = 0;
    let mut debug_frame = None;
    let mut sections = vec![];
    let mut vector_table = None;
    for sect in elf.sections() {
        // If this section resides in RAM, track the highest RAM address in use.
        if let Some(ram) = &ram_region {
            if sect.size() != 0 {
                let last_addr = sect.address() + sect.size() - 1;
                let last_addr = last_addr.try_into()?;
                if ram.range.contains(&last_addr) {
                    log::debug!(
                        "section `{}` is in RAM at 0x{:08X}-0x{:08X}",
                        sect.name().unwrap_or("<unknown>"),
                        sect.address(),
                        last_addr,
                    );
                    highest_ram_addr_in_use = highest_ram_addr_in_use.max(last_addr);
                }
            }
        }

        if let Ok(name) = sect.name() {
            if name == ".debug_frame" {
                debug_frame = Some(sect.data()?);
                continue;
            }

            let size = sect.size();
            // skip empty sections
            if candidates.contains(&name) && size != 0 {
                let start = sect.address();
                if size % 4 != 0 || start % 4 != 0 {
                    // we could support unaligned sections but let's not do that now
                    bail!("section `{}` is not 4-byte aligned", name);
                }

                let start = start.try_into()?;
                let data = sect
                    .data()?
                    .chunks_exact(4)
                    .map(|chunk| u32::from_le_bytes(*array_ref!(chunk, 0, 4)))
                    .collect::<Vec<_>>();

                if name == ".vector_table" {
                    vector_table = Some(VectorTable {
                        location: start,
                        // Initial stack pointer
                        initial_sp: data[0],
                        reset: data[1],
                        hard_fault: data[3],
                    });
                }

                sections.push(Section { start, data });
            }
        }
    }

    let text = elf
        .section_by_name(".text")
        .map(|section| section.index())
        .ok_or_else(|| {
            anyhow!(
            "`.text` section is missing, please make sure that the linker script was passed to the \
            linker (check `.cargo/config.toml` and the `RUSTFLAGS` variable)"
        )
        })?;

    let live_functions = elf
        .symbol_map()
        .symbols()
        .iter()
        .filter_map(|sym| {
            if sym.section() == SymbolSection::Section(text) {
                sym.name()
            } else {
                None
            }
        })
        .collect::<HashSet<_>>();

    let pc2frames = pc2frames::from(&elf, &live_functions)?;
    let (rtt_addr, uses_heap) = rtt_and_heap_info_from(&elf)?;

    let vector_table = vector_table.ok_or_else(|| anyhow!("`.vector_table` section is missing"))?;
    log::debug!("vector table: {:x?}", vector_table);
    let sp_ram_region = target
        .memory_map
        .iter()
        .filter_map(|region| match region {
            MemoryRegion::Ram(region) => {
                // NOTE stack is full descending; meaning the stack pointer can be `ORIGIN(RAM) +
                // LENGTH(RAM)`
                let range = region.range.start..=region.range.end;
                if range.contains(&vector_table.initial_sp) {
                    Some(region)
                } else {
                    None
                }
            }
            _ => None,
        })
        .next()
        .cloned();

    let probes = Probe::list_all();
    let probes = if let Some(probe_opt) = opts.probe.as_deref() {
        let selector = probe_opt.try_into()?;
        probes_filter(&probes, &selector)
    } else {
        probes
    };
    if probes.is_empty() {
        bail!("no probe was found")
    }
    log::debug!("found {} probes", probes.len());
    if probes.len() > 1 {
        bail!("more than one probe found; use --probe to specify which one to use");
    }
    let probe = probes[0].open()?;
    log::debug!("opened probe");
    let mut sess = probe.attach(target)?;
    log::debug!("started session");

    if opts.no_flash {
        log::info!("skipped flashing");
    } else {
        // program lives in Flash
        log::info!("flashing program");
        flashing::download_file(&mut sess, elf_path, Format::Elf)?;
        log::info!("success!");
    }

    let mut canary = None;
    {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

        // Decide if and where to place the stack canary.
        if let Some(ram) = &ram_region {
            // Initial SP must be past canary location.
            let initial_sp_makes_sense = ram.range.contains(&(vector_table.initial_sp - 1))
                && highest_ram_addr_in_use < vector_table.initial_sp;
            if highest_ram_addr_in_use != 0 && !uses_heap && initial_sp_makes_sense {
                let stack_available = vector_table.initial_sp - highest_ram_addr_in_use - 1;

                // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb
                // since filling a lot of RAM is slow (and 1 kb should be "good enough" for what
                // we're doing).
                let canary_size = cmp::min(stack_available / 10, 1024);

                log::debug!(
                    "{} bytes of stack available (0x{:08X}-0x{:08X}), using {} byte canary to detect overflows",
                    stack_available,
                    highest_ram_addr_in_use + 1,
                    vector_table.initial_sp,
                    canary_size,
                );

                // Canary starts right after `highest_ram_addr_in_use`.
                let canary_addr = highest_ram_addr_in_use + 1;
                canary = Some((canary_addr, canary_size));
                let data = vec![STACK_CANARY; canary_size as usize];
                core.write_8(canary_addr, &data)?;
            }
        }

        log::debug!("starting device");
        if core.get_available_breakpoint_units()? == 0 {
            log::warn!("device doesn't support HW breakpoints; HardFault will NOT make `probe-run` exit with an error code");
        } else {
            core.set_hw_breakpoint(vector_table.hard_fault & !THUMB_BIT)?;
        }
        core.run()?;
    }

    // Print a separator before the device messages start.
    eprintln!("{}", "─".repeat(80).dimmed());

    let exit = Arc::new(AtomicBool::new(false));
    let sig_id = signal_hook::flag::register(signal_hook::SIGINT, exit.clone())?;

    let sess = Arc::new(Mutex::new(sess));
    let mut logging_channel = setup_logging_channel(rtt_addr, sess.clone())?;

    // wait for breakpoint
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut read_buf = [0; 1024];
    #[cfg(feature = "defmt")]
    let mut frames = vec![];
    let mut was_halted = false;
    #[cfg(feature = "defmt")]
    let current_dir = std::env::current_dir()?;
    // TODO strip prefix from crates-io paths (?)
    while !exit.load(Ordering::Relaxed) {
        if let Some(logging_channel) = &mut logging_channel {
            let num_bytes_read = match logging_channel.read(&mut read_buf) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("RTT error: {}", e);
                    break;
                }
            };

            if num_bytes_read != 0 {
                match () {
                    #[cfg(feature = "defmt")]
                    () => {
                        if opts.defmt {
                            frames.extend_from_slice(&read_buf[..num_bytes_read]);

                            while let Ok((frame, consumed)) =
                                defmt_decoder::decode(&frames, table.as_ref().unwrap())
                            {
                                // NOTE(`[]` indexing) all indices in `table` have already been
                                // verified to exist in the `locs` map
                                let loc = locs.as_ref().map(|locs| &locs[&frame.index()]);

                                let (mut file, mut line, mut mod_path) = (None, None, None);
                                if let Some(loc) = loc {
                                    let relpath =
                                        if let Ok(relpath) = loc.file.strip_prefix(&current_dir) {
                                            relpath
                                        } else {
                                            // not relative; use full path
                                            &loc.file
                                        };
                                    file = Some(relpath.display().to_string());
                                    line = Some(loc.line as u32);
                                    mod_path = Some(loc.module.clone());
                                }

                                // Forward the defmt frame to our logger.
                                logger::log_defmt(
                                    &frame,
                                    file.as_deref(),
                                    line,
                                    mod_path.as_ref().map(|s| &**s),
                                );

                                let num_frames = frames.len();
                                frames.rotate_left(consumed);
                                frames.truncate(num_frames - consumed);
                            }
                        } else {
                            stdout.write_all(&read_buf[..num_bytes_read])?;
                        }
                    }
                    #[cfg(not(feature = "defmt"))]
                    () => {
                        stdout.write_all(&read_buf[..num_bytes_read])?;
                    }
                }
            }
        }

        let mut sess = sess.lock().unwrap();
        let mut core = sess.core(0)?;
        let is_halted = core.core_halted()?;

        if is_halted && was_halted {
            break;
        }
        was_halted = is_halted;
    }
    drop(stdout);

    // Restore default Ctrl+C behavior.
    signal_hook::unregister(sig_id);
    signal_hook::cleanup::cleanup_signal(signal_hook::SIGINT)?;

    let mut sess = sess.lock().unwrap();
    let mut core = sess.core(0)?;

    if exit.load(Ordering::Relaxed) {
        // Ctrl-C was pressed; stop the microcontroller.
        core.halt(TIMEOUT)?;
    }

    if let Some((addr, len)) = canary {
        let mut buf = vec![0; len as usize];
        core.read_8(addr as u32, &mut buf)?;

        if let Some(pos) = buf.iter().position(|b| *b != STACK_CANARY) {
            let touched_addr = addr + pos as u32;
            log::debug!("canary was touched at 0x{:08X}", touched_addr);

            let min_stack_usage = vector_table.initial_sp - touched_addr;
            log::warn!(
                "program has used at least {} bytes of stack space, data segments \
                may be corrupted due to stack overflow",
                min_stack_usage,
            );
        } else {
            log::debug!("stack canary intact");
        }
    }

    let pc = core.read_core_reg(PC)?;

    let debug_frame = debug_frame.ok_or_else(|| anyhow!("`.debug_frame` section not found"))?;

    // print backtrace
    let top_exception = backtrace(
        &mut core,
        pc,
        debug_frame,
        &elf,
        &pc2frames,
        &vector_table,
        &sp_ram_region,
    )?;

    core.reset_and_halt(TIMEOUT)?;

    Ok(
        if let Some(TopException::HardFault { stack_overflow }) = top_exception {
            if stack_overflow {
                log::error!("the program has overflowed its stack");
            }

            SIGABRT
        } else {
            0
        },
    )
}

const SIGABRT: i32 = 134;

#[derive(Debug, PartialEq)]
enum TopException {
    HardFault { stack_overflow: bool },
    Other,
}

fn setup_logging_channel(
    rtt_addr: Option<u32>,
    sess: Arc<Mutex<Session>>,
) -> Result<Option<UpChannel>, anyhow::Error> {
    if let Some(rtt_addr_res) = rtt_addr {
        const NUM_RETRIES: usize = 10; // picked at random, increase if necessary
        let mut rtt_res: Result<Rtt, probe_rs_rtt::Error> =
            Err(probe_rs_rtt::Error::ControlBlockNotFound);

        for try_index in 0..=NUM_RETRIES {
            rtt_res = Rtt::attach_region(sess.clone(), &ScanRegion::Exact(rtt_addr_res));
            match rtt_res {
                Ok(_) => {
                    log::debug!("Successfully attached RTT");
                    break;
                }
                Err(probe_rs_rtt::Error::ControlBlockNotFound) => {
                    if try_index < NUM_RETRIES {
                        log::trace!("Could not attach because the target's RTT control block isn't initialized (yet). retrying");
                    } else {
                        log::error!("Max number of RTT attach retries exceeded.");
                        return Err(anyhow!(probe_rs_rtt::Error::ControlBlockNotFound));
                    }
                }
                Err(e) => {
                    return Err(anyhow!(e));
                }
            }
        }

        let channel = rtt_res
            .expect("unreachable") // this block is only executed when rtt was successfully attached before
            .up_channels()
            .take(0)
            .ok_or_else(|| anyhow!("RTT up channel 0 not found"))?;
        Ok(Some(channel))
    } else {
        eprintln!("RTT logs not available; blocking until the device halts..");
        Ok(None)
    }
}

fn gimli2probe(reg: &gimli::Register) -> CoreRegisterAddress {
    CoreRegisterAddress(reg.0)
}

struct Registers<'c, 'probe> {
    cache: BTreeMap<u16, u32>,
    core: &'c mut Core<'probe>,
}

impl<'c, 'probe> Registers<'c, 'probe> {
    fn new(lr: u32, sp: u32, core: &'c mut Core<'probe>) -> Self {
        let mut cache = BTreeMap::new();
        cache.insert(LR.0, lr);
        cache.insert(SP.0, sp);
        Self { cache, core }
    }

    fn get(&mut self, reg: CoreRegisterAddress) -> Result<u32, anyhow::Error> {
        Ok(match self.cache.entry(reg.0) {
            btree_map::Entry::Occupied(entry) => *entry.get(),
            btree_map::Entry::Vacant(entry) => *entry.insert(self.core.read_core_reg(reg)?),
        })
    }

    fn insert(&mut self, reg: CoreRegisterAddress, val: u32) {
        self.cache.insert(reg.0, val);
    }

    fn update_cfa(
        &mut self,
        rule: &CfaRule<EndianSlice<LittleEndian>>,
    ) -> Result</* cfa_changed: */ bool, anyhow::Error> {
        match rule {
            CfaRule::RegisterAndOffset { register, offset } => {
                let cfa = (i64::from(self.get(gimli2probe(register))?) + offset) as u32;
                let old_cfa = self.cache.get(&SP.0);
                let changed = old_cfa != Some(&cfa);
                if changed {
                    log::debug!("update_cfa: CFA changed {:8x?} -> {:8x}", old_cfa, cfa);
                }
                self.cache.insert(SP.0, cfa);
                Ok(changed)
            }

            // NOTE not encountered in practice so far
            CfaRule::Expression(_) => todo!("CfaRule::Expression"),
        }
    }

    fn update(
        &mut self,
        reg: &gimli::Register,
        rule: &RegisterRule<EndianSlice<LittleEndian>>,
    ) -> Result<(), anyhow::Error> {
        match rule {
            RegisterRule::Undefined => unreachable!(),

            RegisterRule::Offset(offset) => {
                let cfa = self.get(SP)?;
                let addr = (i64::from(cfa) + offset) as u32;
                self.cache.insert(reg.0, self.core.read_word_32(addr)?);
            }

            _ => unimplemented!(),
        }

        Ok(())
    }
}

fn backtrace(
    core: &mut Core<'_>,
    mut pc: u32,
    debug_frame: &[u8],
    elf: &ElfFile,
    pc2frames: &pc2frames::Map,
    vector_table: &VectorTable,
    sp_ram_region: &Option<RamRegion>,
) -> Result<Option<TopException>, anyhow::Error> {
    let mut debug_frame = DebugFrame::new(debug_frame, LittleEndian);
    // 32-bit ARM -- this defaults to the host's address size which is likely going to be 8
    debug_frame.set_address_size(mem::size_of::<u32>() as u8);

    let sp = core.read_core_reg(SP)?;
    let lr = core.read_core_reg(LR)?;

    // statically linked binary -- there are no relative addresses
    let bases = &BaseAddresses::default();
    let ctx = &mut UninitializedUnwindContext::new();

    let mut top_exception = None;
    let mut frame_index = 0;
    let mut registers = Registers::new(lr, sp, core);
    println!("stack backtrace:");
    loop {
        let mut frames = pc2frames.query_point(pc as u64).collect::<Vec<_>>();
        // `IntervalTree` docs don't specify the order of the elements returned by `query_point` so
        // we sort them by depth before printing
        frames.sort_by_key(|frame| -frame.value.depth);

        if frames.is_empty() {
            // this means there was no DWARF info associated to this PC; the PC could point into
            // external assembly or external C code. Fall back to a symtab lookup
            let map = elf.symbol_map();
            if let Some(name) = map.get(pc as u64).and_then(|symbol| symbol.name()) {
                println!("{:>4}: {}", frame_index, name);
            } else {
                println!("{:>4}: <unknown> @ {:#012x}", frame_index, pc);
            }
            frame_index += 1;
        } else {
            for frame in frames {
                println!("{:>4}: {}", frame_index, frame.value.name);
                frame_index += 1;
            }
        }

        // on hard fault exception entry we hit the breakpoint before the subroutine prelude (`push
        // lr`) is executed so special handling is required
        // also note that hard fault will always be the first frame we unwind
        if top_exception.is_none() {
            top_exception = Some(if pc & !THUMB_BIT == vector_table.hard_fault & !THUMB_BIT {
                // HardFaultTrampoline
                println!("      <exception entry>");

                let stack_overflow = if let Some(sp_ram_region) = sp_ram_region {
                    // NOTE stack is full descending; meaning the stack pointer can be `ORIGIN(RAM) +
                    // LENGTH(RAM)`
                    let range = sp_ram_region.range.start..=sp_ram_region.range.end;
                    !range.contains(&sp)
                } else {
                    log::warn!(
                        "no RAM region appears to contain the stack; cannot determine if this was a stack overflow"
                    );

                    false
                };

                TopException::HardFault { stack_overflow }
            } else {
                TopException::Other
            });
        }

        let uwt_row = debug_frame.unwind_info_for_address(bases, ctx, pc.into(), DebugFrame::cie_from_offset).with_context(|| {
            "debug information is missing. Likely fixes:
1. compile the Rust code with `debug = 1` or higher. This is configured in the `profile.*` section of Cargo.toml
2. use a recent version of the `cortex-m` crates (e.g. cortex-m 0.6.3 or newer). Check versions in Cargo.lock
3. if linking to C code, compile the C code with the `-g` flag"
        })?;

        let cfa_changed = registers.update_cfa(uwt_row.cfa())?;

        for (reg, rule) in uwt_row.registers() {
            registers.update(reg, rule)?;
        }

        let lr = registers.get(LR)?;
        log::debug!("lr=0x{:08x} pc=0x{:08x}", lr, pc);
        if lr == LR_END {
            break;
        }

        // If the frame didn't move, and the program counter didn't change, bail out (otherwise we
        // might print the same frame over and over).
        // Since we strip the thumb bit from `pc`, ignore it in this comparison.
        if !cfa_changed && lr & !THUMB_BIT == pc & !THUMB_BIT {
            println!("error: the stack appears to be corrupted beyond this point");
            return Ok(top_exception);
        }

        if lr > 0xffff_ffe0 {
            let fpu = match lr {
                0xFFFFFFF1 | 0xFFFFFFF9 | 0xFFFFFFFD => false,
                0xFFFFFFE1 | 0xFFFFFFE9 | 0xFFFFFFED => true,
                _ => bail!("LR contains invalid EXC_RETURN value 0x{:08X}", lr),
            };

            println!("      <exception entry>");

            let sp = registers.get(SP)?;
            let stacked = Stacked::read(registers.core, sp, fpu)?;

            registers.insert(LR, stacked.lr);
            // adjust the stack pointer for stacked registers
            registers.insert(SP, sp + stacked.size());
            pc = stacked.pc;
        } else {
            if lr & 1 == 0 {
                bail!("bug? LR ({:#010x}) didn't have the Thumb bit set", lr)
            }
            pc = lr & !THUMB_BIT;
        }
    }

    Ok(top_exception)
}

fn probes_filter(probes: &[DebugProbeInfo], selector: &DebugProbeSelector) -> Vec<DebugProbeInfo> {
    probes
        .iter()
        .filter(|&p| {
            p.vendor_id == selector.vendor_id
                && p.product_id == selector.product_id
                && (selector.serial_number == None || p.serial_number == selector.serial_number)
        })
        .map(|p| p.clone())
        .collect()
}

fn print_probes() -> Result<i32, anyhow::Error> {
    let probes = Probe::list_all();

    if !probes.is_empty() {
        println!("The following devices were found:");
        probes
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!("[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }

    Ok(0)
}

fn print_chips() -> Result<i32, anyhow::Error> {
    let registry = registry::families().expect("Could not retrieve chip family registry");
    for chip_family in registry {
        println!("{}", chip_family.name);
        println!("    Variants:");
        for variant in chip_family.variants.iter() {
            println!("        {}", variant.name);
        }
    }

    Ok(0)
}

#[derive(Debug)]
struct StackedFpuRegs {
    s0: f32,
    s1: f32,
    s2: f32,
    s3: f32,
    s4: f32,
    s5: f32,
    s6: f32,
    s7: f32,
    s8: f32,
    s9: f32,
    s10: f32,
    s11: f32,
    s12: f32,
    s13: f32,
    s14: f32,
    s15: f32,
    fpscr: u32,
}

/// Registers stacked on exception entry.
#[derive(Debug)]
struct Stacked {
    r0: u32,
    r1: u32,
    r2: u32,
    r3: u32,
    r12: u32,
    lr: u32,
    pc: u32,
    xpsr: u32,
    fpu_regs: Option<StackedFpuRegs>,
}

impl Stacked {
    /// Number of 32-bit words stacked in a basic frame.
    const WORDS_BASIC: usize = 8;

    /// Number of 32-bit words stacked in an extended frame.
    const WORDS_EXTENDED: usize = Self::WORDS_BASIC + 17; // 16 FPU regs + 1 status word

    fn read(core: &mut Core<'_>, sp: u32, fpu: bool) -> Result<Self, anyhow::Error> {
        let mut storage = [0; Self::WORDS_EXTENDED];
        let registers: &mut [_] = if fpu {
            &mut storage
        } else {
            &mut storage[..Self::WORDS_BASIC]
        };
        core.read_32(sp, registers)?;

        Ok(Stacked {
            r0: registers[0],
            r1: registers[1],
            r2: registers[2],
            r3: registers[3],
            r12: registers[4],
            lr: registers[5],
            pc: registers[6],
            xpsr: registers[7],
            fpu_regs: if fpu {
                Some(StackedFpuRegs {
                    s0: f32::from_bits(registers[8]),
                    s1: f32::from_bits(registers[9]),
                    s2: f32::from_bits(registers[10]),
                    s3: f32::from_bits(registers[11]),
                    s4: f32::from_bits(registers[12]),
                    s5: f32::from_bits(registers[13]),
                    s6: f32::from_bits(registers[14]),
                    s7: f32::from_bits(registers[15]),
                    s8: f32::from_bits(registers[16]),
                    s9: f32::from_bits(registers[17]),
                    s10: f32::from_bits(registers[18]),
                    s11: f32::from_bits(registers[19]),
                    s12: f32::from_bits(registers[20]),
                    s13: f32::from_bits(registers[21]),
                    s14: f32::from_bits(registers[22]),
                    s15: f32::from_bits(registers[23]),
                    fpscr: registers[24],
                })
            } else {
                None
            },
        })
    }

    /// Returns the in-memory size of these stacked registers, in Bytes.
    fn size(&self) -> u32 {
        let num_words = if self.fpu_regs.is_none() {
            Self::WORDS_BASIC
        } else {
            Self::WORDS_EXTENDED
        };

        num_words as u32 * 4
    }
}

fn rtt_and_heap_info_from(
    elf: &ElfFile,
) -> Result<(Option<u32>, bool /* uses heap */), anyhow::Error> {
    let mut rtt = None;
    let mut uses_heap = false;

    for (_, symbol) in elf.symbols() {
        let name = match symbol.name() {
            Some(name) => name,
            None => continue,
        };

        match name {
            "_SEGGER_RTT" => rtt = Some(symbol.address() as u32),
            "__rust_alloc" | "__rg_alloc" | "__rdl_alloc" | "malloc" if !uses_heap => {
                log::debug!("symbol `{}` indicates heap is in use", name);
                uses_heap = true;
            }
            _ => {}
        }
    }

    Ok((rtt, uses_heap))
}

const LR: CoreRegisterAddress = CoreRegisterAddress(14);
const PC: CoreRegisterAddress = CoreRegisterAddress(15);
const SP: CoreRegisterAddress = CoreRegisterAddress(13);

const LR_END: u32 = 0xFFFF_FFFF;

/// ELF section to be loaded onto the target
#[derive(Debug)]
struct Section {
    start: u32,
    data: Vec<u32>,
}

/// The contents of the vector table
#[derive(Debug)]
struct VectorTable {
    location: u32,
    // entry 0
    initial_sp: u32,
    // entry 1: Reset handler
    reset: u32,
    // entry 3: HardFault handler
    hard_fault: u32,
}
