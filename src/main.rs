mod backtrace;
mod cortexm;
mod registers;
mod stacked;

use std::{
    collections::HashSet,
    convert::TryInto,
    env, fs,
    io::{self, Write as _},
    path::PathBuf,
    process,
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail};
use arrayref::array_ref;
use colored::Colorize as _;
use defmt_decoder::DEFMT_VERSION;
use log::Level;
use object::{
    read::{File as ElfFile, Object as _, ObjectSection as _},
    ObjectSegment, ObjectSymbol, SymbolSection,
};
use probe_rs::{
    config::{registry, MemoryRegion},
    flashing::{self, Format},
    DebugProbeInfo, MemoryInterface, Probe, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use signal_hook::consts::signal;
use structopt::{clap::AppSettings, StructOpt};

/// Successfull termination of process.
const EXIT_SUCCESS: i32 = 0;
const STACK_CANARY: u8 = 0xAA;
const SIGABRT: i32 = 134;
const TIMEOUT: Duration = Duration::from_secs(1);

/// A Cargo runner for microcontrollers.
#[derive(StructOpt)]
#[structopt(name = "probe-run", setting = AppSettings::TrailingVarArg)]
struct Opts {
    /// List supported chips and exit.
    #[structopt(long)]
    list_chips: bool,

    /// Lists all the connected probes and exit.
    #[structopt(long)]
    list_probes: bool,

    /// The chip to program.
    #[structopt(long, required_unless_one(&["list-chips", "list-probes", "version"]), env = "PROBE_RUN_CHIP")]
    chip: Option<String>,

    /// The probe to use (eg. `VID:PID`, `VID:PID:Serial`, or just `Serial`).
    #[structopt(long, env = "PROBE_RUN_PROBE")]
    probe: Option<String>,

    /// The probe clock frequency in kHz
    #[structopt(long)]
    speed: Option<u32>,

    /// Path to an ELF firmware file.
    #[structopt(name = "ELF", parse(from_os_str), required_unless_one(&["list-chips", "list-probes", "version"]))]
    elf: Option<PathBuf>,

    /// Skip writing the application binary to flash.
    #[structopt(long, conflicts_with = "defmt")]
    no_flash: bool,

    /// Connect to device when NRST is pressed.
    #[structopt(long)]
    connect_under_reset: bool,

    /// Enable more verbose logging.
    #[structopt(short, long, parse(from_occurrences))]
    verbose: u32,

    /// Prints version information
    #[structopt(short = "V", long)]
    version: bool,

    /// Print a backtrace even if the program ran successfully
    #[structopt(long)]
    force_backtrace: bool,

    /// Configure the number of lines to print before a backtrace gets cut off
    #[structopt(long, default_value = "50")]
    max_backtrace_len: u32,

    /// Whether to compress the paths to crates.io dependencies
    #[structopt(long)]
    compress_cratesio_dep_paths: bool,

    /// Arguments passed after the ELF file path are discarded
    #[structopt(name = "REST")]
    _rest: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    notmain().map(|code| process::exit(code))
}

fn notmain() -> anyhow::Result<i32> {
    let opts: Opts = Opts::from_args();
    let verbose = opts.verbose;

    defmt_decoder::log::init_logger(verbose >= 1, move |metadata| {
        if defmt_decoder::log::is_defmt_frame(metadata) {
            true // We want to display *all* defmt frames.
        } else {
            // Log depending on how often the `--verbose` (`-v`) cli-param is supplied:
            //   * 0: log everything from probe-run, with level "info" or higher
            //   * 1: log everything from probe-run
            //   * 2 or more: log everything
            if verbose >= 2 {
                true
            } else if verbose >= 1 {
                metadata.target().starts_with("probe_run")
            } else {
                metadata.target().starts_with("probe_run") && metadata.level() <= Level::Info
            }
        }
    });

    if opts.version {
        print_version();
        return Ok(EXIT_SUCCESS);
    } else if opts.list_probes {
        print_probes(Probe::list_all());
        return Ok(EXIT_SUCCESS);
    } else if opts.list_chips {
        print_chips();
        return Ok(EXIT_SUCCESS);
    }

    let force_backtrace = opts.force_backtrace;
    let max_backtrace_len = opts.max_backtrace_len;
    let compress_cratesio_dep_paths = opts.compress_cratesio_dep_paths;
    let elf_path = opts.elf.as_deref().unwrap();
    let chip = opts.chip.as_deref().unwrap();
    let bytes = fs::read(elf_path)?;
    let elf = ElfFile::parse(&bytes)?;

    let target = probe_rs::config::registry::get_target_by_name(chip)?;

    // find and report the RAM region
    let mut ram_region = None;
    for region in &target.memory_map {
        if let MemoryRegion::Ram(ram) = region {
            if let Some(old) = &ram_region {
                log::debug!("multiple RAM regions found ({:?} and {:?}), stack canary will not be available", old, ram);
            } else {
                ram_region = Some(ram.clone());
            }
        }
    }
    if let Some(ram) = &ram_region {
        log::debug!(
            "RAM region: 0x{:08X}-0x{:08X}",
            ram.range.start,
            ram.range.end - 1
        );
    }
    let ram_region = ram_region;

    // NOTE we want to raise the linking error before calling `defmt_decoder::Table::parse`
    let text = elf
        .section_by_name(".text")
        .map(|section| section.index())
        .ok_or_else(|| {
            anyhow!(
                "`.text` section is missing, please make sure that the linker script was passed \
                to the linker (check `.cargo/config.toml` and the `RUSTFLAGS` variable)"
            )
        })?;

    // Parse defmt_decoder-table from bytes
    // * skip defmt version check, if `PROBE_RUN_IGNORE_VERSION` matches one of the options
    let mut table = match env::var("PROBE_RUN_IGNORE_VERSION").as_deref() {
        Ok("true") | Ok("1") => defmt_decoder::Table::parse_ignore_version(&bytes)?,
        _ => defmt_decoder::Table::parse(&bytes)?,
    };
    // Extract the `Locations` from the table, if there is a table
    let mut locs = None;
    if let Some(table) = table.as_ref() {
        let tmp = table.get_locations(&bytes)?;

        if !table.is_empty() && tmp.is_empty() {
            log::warn!("insufficient DWARF info; compile your program with `debug = 2` to enable location info");
        } else if table.indices().all(|idx| tmp.contains_key(&(idx as u64))) {
            locs = Some(tmp);
        } else {
            log::warn!("(BUG) location info is incomplete; it will be omitted from the output");
        }
    }
    let locs = locs;

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
    let (debug_frame, vector_table) = (debug_frame, vector_table);

    let live_functions = elf
        .symbols()
        .filter_map(|sym| {
            if sym.section() == SymbolSection::Section(text) {
                Some(sym.name())
            } else {
                None
            }
        })
        .collect::<Result<HashSet<_>, _>>()?;

    let (rtt_addr, uses_heap, main) = get_rtt_heap_main_from(&elf)?;

    let vector_table = vector_table.ok_or_else(|| anyhow!("`.vector_table` section is missing"))?;
    log::debug!("vector table: {:x?}", vector_table);
    let sp_ram_region = target
        .memory_map
        .iter()
        .filter_map(|region| match region {
            MemoryRegion::Ram(region) => {
                // NOTE stack is full descending; meaning the stack pointer can be
                // `ORIGIN(RAM) + LENGTH(RAM)`
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
        let selector = probe_opt.parse()?;
        probes_filter(&probes, &selector)
    } else {
        probes
    };

    // ensure exactly one probe is found and open it
    if probes.is_empty() {
        bail!("no probe was found")
    }
    log::debug!("found {} probes", probes.len());
    if probes.len() > 1 {
        let _ = print_probes(probes);
        bail!("more than one probe found; use --probe to specify which one to use");
    }
    let mut probe = probes[0].open()?;
    log::debug!("opened probe");

    if let Some(speed) = opts.speed {
        probe.set_speed(speed)?;
    }

    let mut sess = if opts.connect_under_reset {
        probe.attach_under_reset(target)?
    } else {
        probe.attach(target)?
    };
    log::debug!("started session");

    if opts.no_flash {
        log::info!("skipped flashing");
    } else {
        // program lives in Flash
        let size = program_size_of(&elf);
        log::info!("flashing program ({:.02} KiB)", size as f64 / 1024.0);
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

                // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb since
                // filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're doing).
                let canary_size = 1024.min(stack_available / 10);

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
            if rtt_addr.is_some() {
                bail!("RTT not supported on device without HW breakpoints");
            } else {
                log::warn!("device doesn't support HW breakpoints; HardFault will NOT make `probe-run` exit with an error code");
            }
        }

        if let Some(rtt) = rtt_addr {
            core.set_hw_breakpoint(main)?;
            core.run()?;
            core.wait_for_core_halted(Duration::from_secs(5))?;
            const OFFSET: u32 = 44;
            const FLAG: u32 = 2; // BLOCK_IF_FULL
            core.write_word_32(rtt + OFFSET, FLAG)?;
            core.clear_hw_breakpoint(main)?;
        }

        core.set_hw_breakpoint(cortexm::clear_thumb_bit(vector_table.hard_fault))?;
        core.run()?;
    }
    let canary = canary;

    // Register a signal handler that sets `exit` to `true` on Ctrl+C. On the second Ctrl+C, the
    // signal's default action will be run.
    let exit = Arc::new(AtomicBool::new(false));
    let sigid = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

    let sess = Arc::new(Mutex::new(sess));
    let mut logging_channel = setup_logging_channel(rtt_addr, sess.clone())?;

    // `defmt-rtt` names the channel "defmt", so enable defmt decoding in that case.
    let use_defmt = logging_channel
        .as_ref()
        .map_or(false, |ch| ch.name() == Some("defmt"));

    if use_defmt && opts.no_flash {
        bail!(
            "attempted to use `--no-flash` and `defmt` logging -- this combination is not allowed. Remove the `--no-flash` flag"
        );
    } else if use_defmt && table.is_none() {
        bail!("\"defmt\" RTT channel is in use, but the firmware binary contains no defmt data");
    }

    if !use_defmt {
        table = None;
    }

    print_separator();

    // wait for breakpoint
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut read_buf = [0; 1024];
    let mut frames = vec![];
    let mut was_halted = false;
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
                if let Some(table) = table.as_ref() {
                    frames.extend_from_slice(&read_buf[..num_bytes_read]);

                    loop {
                        match table.decode(&frames) {
                            Ok((frame, consumed)) => {
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
                                defmt_decoder::log::log_defmt(
                                    &frame,
                                    file.as_deref(),
                                    line,
                                    mod_path.as_deref(),
                                );

                                let num_frames = frames.len();
                                frames.rotate_left(consumed);
                                frames.truncate(num_frames - consumed);
                            }
                            Err(defmt_decoder::DecodeError::UnexpectedEof) => break,
                            Err(defmt_decoder::DecodeError::Malformed) => {
                                log::error!("failed to decode defmt data: {:x?}", frames);
                                return Err(defmt_decoder::DecodeError::Malformed.into());
                            }
                        }
                    }
                } else {
                    stdout.write_all(&read_buf[..num_bytes_read])?;
                    stdout.flush()?;
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

    // Make any incoming SIGINT terminate the process.
    // Due to https://github.com/vorner/signal-hook/issues/97, this will result in SIGABRT, but you
    // only need to Ctrl+C here if the backtrace hangs, so that should be fine.
    signal_hook::low_level::unregister(sigid);
    signal_hook::flag::register_conditional_default(signal::SIGINT, exit.clone())?;

    let mut sess = sess.lock().unwrap();
    let mut core = sess.core(0)?;

    if exit.load(Ordering::Relaxed) {
        // Ctrl-C was pressed; stop the microcontroller.
        core.halt(TIMEOUT)?;
    }

    // TODO move into own function?
    let mut canary_touched = false;
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
            canary_touched = true;
        } else {
            log::debug!("stack canary intact");
        }
    }

    let debug_frame = debug_frame.ok_or_else(|| anyhow!("`.debug_frame` section not found"))?;

    print_separator();

    let halted_due_to_signal = exit.load(Ordering::Relaxed);
    let backtrace_settings = backtrace::Settings {
        current_dir: &current_dir,
        max_backtrace_len,
        // TODO any other cases in which we should force a backtrace?
        force_backtrace: force_backtrace || canary_touched || halted_due_to_signal,
        compress_cratesio_dep_paths,
    };

    let outcome = backtrace::print(
        &mut core,
        debug_frame,
        &elf,
        &vector_table,
        &sp_ram_region,
        &live_functions,
        &backtrace_settings,
    )?;

    core.reset_and_halt(TIMEOUT)?;

    Ok(match outcome {
        Outcome::StackOverflow => {
            log::error!("the program has overflowed its stack");
            SIGABRT
        }
        Outcome::HardFault => {
            log::error!("the program panicked");
            SIGABRT
        }
        Outcome::Ok => {
            log::info!("device halted without error");
            0
        }
    })
}

fn program_size_of(file: &ElfFile) -> u64 {
    // `segments` iterates only over *loadable* segments,
    // which are the segments that will be loaded to Flash by probe-rs
    file.segments().map(|segment| segment.size()).sum()
}

#[derive(Debug, PartialEq)]
pub enum TopException {
    StackOverflow,
    HardFault, // generic hard fault
}

fn setup_logging_channel(
    rtt_addr: Option<u32>,
    sess: Arc<Mutex<Session>>,
) -> anyhow::Result<Option<UpChannel>> {
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

struct ProbeFilter {
    vid_pid: Option<(u16, u16)>,
    serial: Option<String>,
}

impl FromStr for ProbeFilter {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split(':').collect::<Vec<_>>();
        match &*parts {
            [serial] => Ok(Self {
                vid_pid: None,
                serial: Some(serial.to_string()),
            }),
            [vid, pid] => Ok(Self {
                vid_pid: Some((u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?)),
                serial: None,
            }),
            [vid, pid, serial] => Ok(Self {
                vid_pid: Some((u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?)),
                serial: Some(serial.to_string()),
            }),
            _ => Err(anyhow!("invalid probe filter")),
        }
    }
}

fn probes_filter(probes: &[DebugProbeInfo], selector: &ProbeFilter) -> Vec<DebugProbeInfo> {
    probes
        .iter()
        .filter(|&p| {
            if let Some((vid, pid)) = selector.vid_pid {
                if p.vendor_id != vid || p.product_id != pid {
                    return false;
                }
            }

            if let Some(serial) = &selector.serial {
                if p.serial_number.as_deref() != Some(serial) {
                    return false;
                }
            }

            true
        })
        .cloned()
        .collect()
}

fn print_chips() {
    let registry = registry::families().expect("Could not retrieve chip family registry");
    for chip_family in registry {
        println!("{}\n    Variants:", chip_family.name);
        for variant in chip_family.variants.iter() {
            println!("        {}", variant.name);
        }
    }
}

fn print_probes(probes: Vec<DebugProbeInfo>) {
    if !probes.is_empty() {
        println!("The following devices were found:");
        probes
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!("[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }
}

/// The string reported by the `--version` flag
fn print_version() {
    const VERSION: &str = env!("CARGO_PKG_VERSION"); // version from Cargo.toml e.g. "0.1.4"
    const HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/git-info.txt")); // "" OR git hash e.g. "34019f8" -- this is generated in build.rs
    println!(
        "{}{}\nsupported defmt version: {}",
        VERSION, HASH, DEFMT_VERSION
    );
}

/// Print a line to separate different execution stages.
fn print_separator() {
    println!("{}", "─".repeat(80).dimmed());
}

fn get_rtt_heap_main_from(
    elf: &ElfFile,
) -> anyhow::Result<(Option<u32>, /* uses heap: */ bool, u32)> {
    let mut rtt = None;
    let mut uses_heap = false;
    let mut main = None;

    for symbol in elf.symbols() {
        let name = match symbol.name() {
            Ok(name) => name,
            Err(_) => continue,
        };

        match name {
            "main" => main = Some(cortexm::clear_thumb_bit(symbol.address() as u32)),
            "_SEGGER_RTT" => rtt = Some(symbol.address() as u32),
            "__rust_alloc" | "__rg_alloc" | "__rdl_alloc" | "malloc" if !uses_heap => {
                log::debug!("symbol `{}` indicates heap is in use", name);
                uses_heap = true;
            }
            _ => {}
        }
    }

    Ok((
        rtt,
        uses_heap,
        main.ok_or_else(|| anyhow!("`main` symbol not found"))?,
    ))
}

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

/// Target program outcome
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    HardFault,
    Ok,
    StackOverflow,
}
