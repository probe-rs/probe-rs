mod registers;
mod stacked;

use std::{
    borrow::Cow,
    collections::HashSet,
    convert::TryInto,
    fs,
    io::{self, Write as _},
    mem,
    path::{Path, PathBuf},
    process,
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use addr2line::fallible_iterator::FallibleIterator as _;
use anyhow::{anyhow, bail, Context};
use arrayref::array_ref;
use colored::Colorize as _;
use defmt_decoder::DEFMT_VERSION;
use gimli::{
    read::{DebugFrame, UnwindSection},
    BaseAddresses, LittleEndian, UninitializedUnwindContext,
};
use log::Level;
use object::{
    read::{File as ElfFile, Object as _, ObjectSection as _},
    ObjectSegment, ObjectSymbol, SymbolSection,
};
use probe_rs::{
    config::{registry, MemoryRegion, RamRegion},
    flashing::{self, Format},
    Core, DebugProbeInfo, MemoryInterface, Probe, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use signal_hook::consts::signal;
use structopt::{clap::AppSettings, StructOpt};

use crate::{
    registers::{Registers, LR, LR_END, PC, SP},
    stacked::Stacked,
};

/// Successfull termination of process.
const EXIT_SUCCESS: i32 = 0;
const STACK_CANARY: u8 = 0xAA;
const SIGABRT: i32 = 134;
const THUMB_BIT: u32 = 1;
const TIMEOUT: Duration = Duration::from_secs(1);
const EXC_RETURN_MARKER: u32 = 0xFFFF_FFF0;

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
    let mut table = match option_env!("PROBE_RUN_IGNORE_VERSION") {
        Some("true") | Some("1") => defmt_decoder::Table::parse_ignore_version(&bytes)?,
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

        core.set_hw_breakpoint(vector_table.hard_fault & !THUMB_BIT)?;
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

    let pc = core.read_core_reg(PC)?;

    let debug_frame = debug_frame.ok_or_else(|| anyhow!("`.debug_frame` section not found"))?;

    print_separator();

    let top_exception = construct_backtrace(
        &mut core,
        pc,
        debug_frame,
        &elf,
        &vector_table,
        &sp_ram_region,
        &live_functions,
        &current_dir,
        // TODO any other cases in which we should force a backtrace?
        force_backtrace || canary_touched,
        max_backtrace_len,
    )?;

    core.reset_and_halt(TIMEOUT)?;

    Ok(match top_exception {
        Some(TopException::StackOverflow) => {
            log::error!("the program has overflowed its stack");
            SIGABRT
        }
        Some(TopException::HardFault) => {
            log::error!("the program panicked");
            SIGABRT
        }
        None => {
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
enum TopException {
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

#[allow(clippy::too_many_arguments)] // FIXME: clean this up
fn construct_backtrace(
    core: &mut Core<'_>,
    mut pc: u32,
    debug_frame: &[u8],
    elf: &ElfFile,
    vector_table: &VectorTable,
    sp_ram_region: &Option<RamRegion>,
    live_functions: &HashSet<&str>,
    current_dir: &Path,
    force_backtrace: bool,
    max_backtrace_len: u32,
) -> Result<Option<TopException>, anyhow::Error> {
    let mut debug_frame = DebugFrame::new(debug_frame, LittleEndian);
    // 32-bit ARM -- this defaults to the host's address size which is likely going to be 8
    debug_frame.set_address_size(mem::size_of::<u32>() as u8);

    let sp = core.read_core_reg(SP)?;
    let lr = core.read_core_reg(LR)?;

    // statically linked binary -- there are no relative addresses
    let bases = &BaseAddresses::default();
    let ctx = &mut UninitializedUnwindContext::new();

    let addr2line = addr2line::Context::new(elf)?;
    let mut top_exception = None;
    let mut frame_index = 0;
    let mut registers = Registers::new(lr, sp, core);
    let symtab = elf.symbol_map();
    let mut print_backtrace = force_backtrace;

    loop {
        let frames = addr2line.find_frames(pc as u64)?.collect::<Vec<_>>()?;
        // when the input of `find_frames` is the PC of a subroutine that has no debug information
        // (e.g. external assembly), it will either return an empty `FrameIter` OR the frames that
        // correspond to a subroutine GC-ed by the linker, instead of an `Err`or.
        // To detect the second failure mode we check that the last frame (the non-inline one) is
        // actually "live" (exists in the final binary).
        // When there's no debuginfo we fallback to a symtab lookup to at least provide the name of
        // the function that contains the PC.
        let subroutine = frames.last();
        let has_valid_debuginfo = if let Some(function) =
            subroutine.and_then(|subroutine| subroutine.function.as_ref())
        {
            live_functions.contains(&*function.raw_name()?)
        } else {
            false
        };

        // This is our first run through the loop, some initial handling and printing is required
        // TODO refactor this
        if frame_index == 0 {
            if pc & !THUMB_BIT == vector_table.hard_fault & !THUMB_BIT {
                // HardFaultTrampoline
                // on hard fault exception entry we hit the breakpoint before the subroutine prelude (`push
                // lr`) is executed so special handling is required
                // also note that hard fault will always be the first frame we unwind

                print_backtrace_start();

                let mut stack_overflow = false;

                if let Some(sp_ram_region) = sp_ram_region {
                    // NOTE stack is full descending; meaning the stack pointer can be
                    // `ORIGIN(RAM) + LENGTH(RAM)`
                    let range = sp_ram_region.range.start..=sp_ram_region.range.end;
                    stack_overflow = !range.contains(&sp);

                    // if a stack overflow happened, we're definitely printing a backtrace
                    print_backtrace |= stack_overflow;
                } else {
                    log::warn!(
                        "no RAM region appears to contain the stack; cannot determine if this was a stack overflow"
                    );
                };

                top_exception = Some(match stack_overflow {
                    true => TopException::StackOverflow,
                    false => TopException::HardFault,
                });
            } else {
                if force_backtrace {
                    print_backtrace_start();
                }
            }
        }

        let mut backtrace_display_str = "".to_string();

        if has_valid_debuginfo {
            for frame in &frames {
                let name = frame
                    .function
                    .as_ref()
                    .map(|function| function.demangle())
                    .transpose()?
                    .unwrap_or(Cow::Borrowed("???"));

                backtrace_display_str.push_str(&format!("{:>4}: {}\n", frame_index, name));
                frame_index += 1;

                if let Some((file, line)) = frame
                    .location
                    .as_ref()
                    .and_then(|loc| loc.file.and_then(|file| loc.line.map(|line| (file, line))))
                {
                    let file = Path::new(file);
                    let relpath = if let Ok(relpath) = file.strip_prefix(&current_dir) {
                        relpath
                    } else {
                        // not within current directory; use full path
                        file
                    };
                    backtrace_display_str.push_str(&format!(
                        "        at {}:{}\n",
                        relpath.display(),
                        line
                    ));
                }
            }
        } else {
            // .symtab fallback
            // the .symtab appears to use address ranges that have their thumb bits set (e.g.
            // `0x101..0x200`). Passing the `pc` with the thumb bit cleared (e.g. `0x100`) to the
            // lookup function sometimes returns the *previous* symbol. Work around the issue by
            // setting `pc`'s thumb bit before looking it up
            let address = (pc | THUMB_BIT) as u64;
            let name = symtab
                .get(address)
                .map(|symbol| symbol.name())
                .unwrap_or("???");
            backtrace_display_str.push_str(&format!("{:>4}: {}\n", frame_index, name));
            frame_index += 1;
        }

        if print_backtrace {
            // we need to print everything we've collected up until now, otherwise the
            // debug level logs won't match up
            print!("{}", backtrace_display_str);
        }

        let uwt_row = debug_frame
            .unwind_info_for_address(bases, ctx, pc.into(), DebugFrame::cie_from_offset)
            .with_context(|| {
            "debug information is missing. Likely fixes:
1. compile the Rust code with `debug = 1` or higher. This is configured in the `profile.{release,bench}` sections of Cargo.toml (`profile.{dev,test}` default to `debug = 2`)
2. use a recent version of the `cortex-m` crates (e.g. cortex-m 0.6.3 or newer). Check versions in Cargo.lock
3. if linking to C code, compile the C code with the `-g` flag"
        })?;

        let cfa_changed = registers.update_cfa(uwt_row.cfa())?;

        for (reg, rule) in uwt_row.registers() {
            registers.update(reg, rule)?;
        }

        let lr = registers.get(LR)?;

        if lr == LR_END {
            break;
        }

        // Link Register contains an EXC_RETURN value. This deliberately also includes
        // invalid combinations of final bits 0-4 to prevent futile backtrace re-generation attempts
        let exception_entry = lr >= EXC_RETURN_MARKER;

        // Since we strip the thumb bit from `pc`, ignore it in this comparison.
        let program_counter_changed = (lr & !THUMB_BIT) != (pc & !THUMB_BIT);
        // If the frame didn't move, and the program counter didn't change, bail out (otherwise we
        // might print the same frame over and over).
        let stack_corrupted = !cfa_changed && !program_counter_changed;

        if !print_backtrace && (stack_corrupted || exception_entry) {
            // we haven't printed a backtrace yet but have discovered a corrupted stack or exception:
            // print the backtrace now
            print!("{}", backtrace_display_str);

            // and enforce backtrace printing from this point on
            print_backtrace = true;
        }

        if print_backtrace {
            log::debug!("lr=0x{:08x} pc=0x{:08x}", lr, pc);
        }

        if stack_corrupted {
            println!("error: the stack appears to be corrupted beyond this point");

            if top_exception == Some(TopException::StackOverflow) {
                return Ok(top_exception);
            } else {
                return Ok(Some(TopException::HardFault));
            }
        }

        if exception_entry {
            let fpu = match lr {
                0xFFFFFFF1 | 0xFFFFFFF9 | 0xFFFFFFFD => false,
                0xFFFFFFE1 | 0xFFFFFFE9 | 0xFFFFFFED => true,
                _ => bail!("LR contains invalid EXC_RETURN value 0x{:08X}", lr),
            };

            println!("      <exception entry>");

            let sp = registers.get(SP)?;
            let ram_bounds = sp_ram_region
                .as_ref()
                .map(|ram_region| ram_region.range.clone())
                // if no device-specific information, use the range specific in the Cortex-M* Technical Reference Manual
                .unwrap_or(0x2000_0000..0x4000_0000);
            let stacked = if let Some(stacked) = Stacked::read(registers.core, sp, fpu, ram_bounds)?
            {
                stacked
            } else {
                log::warn!("exception entry pushed registers outside RAM; not possible to unwind the stack");
                return Ok(top_exception);
            };

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

        if frame_index >= max_backtrace_len {
            log::warn!(
                "maximum backtrace length of {} reached; cutting off the rest
               note: re-run with `--max-backtrace-len=<your maximum>` to extend this limit",
                max_backtrace_len
            );
            return Ok(top_exception);
        }
    }

    Ok(top_exception)
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

/// Print a message indicating that the backtrace starts here
fn print_backtrace_start() {
    println!("{}", "stack backtrace:".dimmed());
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
            "main" => main = Some(symbol.address() as u32 & !THUMB_BIT),
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
