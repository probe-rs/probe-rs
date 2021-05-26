mod backtrace;
mod cortexm;
mod dep;
mod elf;
mod registers;
mod stacked;

use std::{
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
use colored::Colorize as _;
use defmt_decoder::DEFMT_VERSION;
use log::Level;
use object::read::{Object as _, ObjectSection as _};
use probe_rs::{
    config::registry,
    flashing::{self, Format},
    DebugProbeInfo, MemoryInterface, Probe, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use signal_hook::consts::signal;
use structopt::{clap::AppSettings, StructOpt};

use crate::elf::ProcessedElf;
use crate::{backtrace::Outcome, elf::TargetInfo};

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

    /// Whether to shorten paths (e.g. to crates.io dependencies) in backtraces and defmt logs
    #[structopt(long)]
    shorten_paths: bool,

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
    let shorten_paths = opts.shorten_paths;
    let elf_path = opts.elf.as_deref().unwrap();
    if !elf_path.exists() {
        return Err(anyhow!(
            "can't find ELF file at `{}`; are you sure you got the right path?",
            elf_path.display()
        ));
    }
    let chip = opts.chip.as_deref().unwrap();
    let bytes = fs::read(elf_path)?;
    let mut elf = ProcessedElf::from_elf(&bytes)?;

    let target_info = TargetInfo::new(chip, elf.vector_table.initial_stack_pointer)?;

    // TODO continue looking at code from here
    log::debug!("vector table: {:x?}", elf.vector_table);

    // TODO use this instead of iterating?
    let mut highest_ram_addr_in_use = 0;
    for sect in elf.sections() {
        // If this section resides in RAM, track the highest RAM address in use.
        if let Some(ram) = &target_info.active_ram_region {
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
    }

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
        probe.attach_under_reset(target_info.target)?
    } else {
        probe.attach(target_info.target)?
    };
    log::debug!("started session");

    if opts.no_flash {
        log::info!("skipped flashing");
    } else {
        // program lives in Flash
        let size = elf.program_size();
        log::info!("flashing program ({:.02} KiB)", size as f64 / 1024.0);
        flashing::download_file(&mut sess, elf_path, Format::Elf)?;
        log::info!("success!");
    }

    let mut canary = None;
    {
        let mut core = sess.core(0)?;
        core.reset_and_halt(TIMEOUT)?;

        // Decide if and where to place the stack canary.
        if let Some(ram) = &target_info.active_ram_region {
            // Initial SP must be past canary location.
            let initial_sp_makes_sense = ram
                .range
                .contains(&(elf.vector_table.initial_stack_pointer - 1))
                && highest_ram_addr_in_use < elf.vector_table.initial_stack_pointer;
            if highest_ram_addr_in_use != 0
                && !elf.target_program_uses_heap
                && initial_sp_makes_sense
            {
                let stack_available =
                    elf.vector_table.initial_stack_pointer - highest_ram_addr_in_use - 1;

                // We consider >90% stack usage a potential stack overflow, but don't go beyond 1 kb since
                // filling a lot of RAM is slow (and 1 kb should be "good enough" for what we're doing).
                let canary_size = 1024.min(stack_available / 10);

                log::debug!(
                    "{} bytes of stack available (0x{:08X}-0x{:08X}), using {} byte canary to detect overflows",
                    stack_available,
                    highest_ram_addr_in_use + 1,
                    elf.vector_table.initial_stack_pointer,
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
            if elf.rtt_buffer_address.is_some() {
                bail!("RTT not supported on device without HW breakpoints");
            } else {
                log::warn!("device doesn't support HW breakpoints; HardFault will NOT make `probe-run` exit with an error code");
            }
        }

        if let Some(rtt) = elf.rtt_buffer_address {
            core.set_hw_breakpoint(elf.main_function_address)?;
            core.run()?;
            core.wait_for_core_halted(Duration::from_secs(5))?;
            const OFFSET: u32 = 44;
            const FLAG: u32 = 2; // BLOCK_IF_FULL
            core.write_word_32(rtt + OFFSET, FLAG)?;
            core.clear_hw_breakpoint(elf.main_function_address)?;
        }

        core.set_hw_breakpoint(cortexm::clear_thumb_bit(elf.vector_table.hard_fault))?;
        core.run()?;
    }
    let canary = canary;

    // Register a signal handler that sets `exit` to `true` on Ctrl+C. On the second Ctrl+C, the
    // signal's default action will be run.
    let exit = Arc::new(AtomicBool::new(false));
    let sigid = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

    let sess = Arc::new(Mutex::new(sess));
    let mut logging_channel = setup_logging_channel(elf.rtt_buffer_address, sess.clone())?;

    // `defmt-rtt` names the channel "defmt", so enable defmt decoding in that case.
    let use_defmt = logging_channel
        .as_ref()
        .map_or(false, |ch| ch.name() == Some("defmt"));

    if use_defmt && opts.no_flash {
        bail!(
            "attempted to use `--no-flash` and `defmt` logging -- this combination is not allowed. Remove the `--no-flash` flag"
        );
    } else if use_defmt && elf.defmt_table.is_none() {
        bail!("\"defmt\" RTT channel is in use, but the firmware binary contains no defmt data");
    }

    if !use_defmt {
        elf.defmt_table = None;
    }

    print_separator();

    // wait for breakpoint ???
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut read_buf = [0; 1024];
    // this is undecoded RTT data
    let mut frames = vec![];
    let mut was_halted = false;
    let current_dir = std::env::current_dir()?;

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
                if let Some(table) = elf.defmt_table.as_ref() {
                    frames.extend_from_slice(&read_buf[..num_bytes_read]);

                    loop {
                        match table.decode(&frames) {
                            Ok((frame, consumed)) => {
                                // NOTE(`[]` indexing) all indices in `table` have already been
                                // verified to exist in the `locs` map
                                let loc = elf
                                    .defmt_locations
                                    .as_ref()
                                    .map(|locs| &locs[&frame.index()]);

                                let (mut file, mut line, mut mod_path) = (None, None, None);
                                if let Some(loc) = loc {
                                    let path =
                                        if let Ok(relpath) = loc.file.strip_prefix(&current_dir) {
                                            relpath.display().to_string()
                                        } else {
                                            let dep_path = dep::Path::from_std_path(&loc.file);

                                            if shorten_paths {
                                                dep_path.format_short()
                                            } else {
                                                dep_path.format_highlight()
                                            }
                                        };

                                    file = Some(path);
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

            let min_stack_usage = elf.vector_table.initial_stack_pointer - touched_addr;
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

    print_separator();

    let halted_due_to_signal = exit.load(Ordering::Relaxed);
    let backtrace_settings = backtrace::Settings {
        current_dir: &current_dir,
        max_backtrace_len,
        // TODO any other cases in which we should force a backtrace?
        force_backtrace: force_backtrace || canary_touched || halted_due_to_signal,
        shorten_paths,
    };

    let outcome = backtrace::print(
        &mut core,
        &elf,
        &target_info.active_ram_region,
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

fn setup_logging_channel(
    rtt_buffer_address: Option<u32>,
    sess: Arc<Mutex<Session>>,
) -> anyhow::Result<Option<UpChannel>> {
    if let Some(rtt_buffer_address) = rtt_buffer_address {
        const NUM_RETRIES: usize = 10; // picked at random, increase if necessary
        let mut rtt_res: Result<Rtt, probe_rs_rtt::Error> =
            Err(probe_rs_rtt::Error::ControlBlockNotFound);

        for try_index in 0..=NUM_RETRIES {
            rtt_res = Rtt::attach_region(sess.clone(), &ScanRegion::Exact(rtt_buffer_address));
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
