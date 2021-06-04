mod backtrace;
mod canary;
mod cli;
mod cortexm;
mod dep;
mod elf;
mod probe;
mod registers;
mod stacked;
mod target_info;

use std::{
    env, fs,
    io::{self, Write as _},
    path::Path,
    process,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail};
use colored::Colorize as _;
use probe_rs::{
    flashing::{self, Format},
    MemoryInterface as _, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use signal_hook::consts::signal;

use crate::{canary::Canary, elf::Elf, target_info::TargetInfo};

const SIGABRT: i32 = 134;
const TIMEOUT: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    cli::handle_arguments().map(|code| process::exit(code))
}

fn run_target_program(elf_path: &Path, chip_name: &str, opts: &cli::Opts) -> anyhow::Result<i32> {
    if !elf_path.exists() {
        return Err(anyhow!(
            "can't find ELF file at `{}`; are you sure you got the right path?",
            elf_path.display()
        ));
    }

    let elf_bytes = fs::read(elf_path)?;
    let mut elf = Elf::parse(&elf_bytes)?;

    let target_info = TargetInfo::new(chip_name, &elf)?;

    let probe = probe::open(opts)?;

    let probe_target = target_info.probe_target.clone();
    let mut sess = if opts.connect_under_reset {
        probe.attach_under_reset(probe_target)?
    } else {
        probe.attach(probe_target)?
    };
    log::debug!("started session");

    if opts.no_flash {
        log::info!("skipped flashing");
    } else {
        let size = elf.program_flash_size();
        log::info!("flashing program ({:.02} KiB)", size as f64 / 1024.0);

        flashing::download_file(&mut sess, elf_path, Format::Elf)?;
        log::info!("success!");
    }

    let canary = Canary::install(&mut sess, &target_info, &elf)?;
    start_program(&mut sess, &elf)?;

    let sess = Arc::new(Mutex::new(sess));
    let current_dir = &env::current_dir()?;

    let halted_due_to_signal = extract_and_print_logs(&mut elf, &sess, opts, current_dir)?;

    let mut sess = sess.lock().unwrap();
    let mut core = sess.core(0)?;

    print_separator();

    let canary_touched = canary
        .map(|canary| canary.touched(&mut core, &elf))
        .transpose()?
        .unwrap_or(false);
    let backtrace_settings = backtrace::Settings {
        current_dir,
        max_backtrace_len: opts.max_backtrace_len,
        force_backtrace: opts.force_backtrace || canary_touched || halted_due_to_signal,
        shorten_paths: opts.shorten_paths,
    };

    let outcome = backtrace::print(
        &mut core,
        &elf,
        &target_info.active_ram_region,
        &backtrace_settings,
    )?;

    core.reset_and_halt(TIMEOUT)?;

    outcome.log();

    Ok(outcome.into())
}

fn start_program(sess: &mut Session, elf: &Elf) -> Result<(), anyhow::Error> {
    let mut core = sess.core(0)?;

    log::debug!("starting device");
    if core.get_available_breakpoint_units()? == 0 {
        if elf.rtt_buffer_address().is_some() {
            bail!("RTT not supported on device without HW breakpoints");
        } else {
            log::warn!("device doesn't support HW breakpoints; HardFault will NOT make `probe-run` exit with an error code");
        }
    }
    if let Some(rtt) = elf.rtt_buffer_address() {
        let main = elf.main_function_address();
        core.set_hw_breakpoint(main)?;
        core.run()?;
        core.wait_for_core_halted(Duration::from_secs(5))?;

        const OFFSET: u32 = 44;
        const FLAG: u32 = 2; // BLOCK_IF_FULL
        core.write_word_32(rtt + OFFSET, FLAG)?;
        core.clear_hw_breakpoint(main)?;
    }
    core.set_hw_breakpoint(cortexm::clear_thumb_bit(elf.vector_table.hard_fault))?;
    core.run()?;
    Ok(())
}

fn extract_and_print_logs(
    elf: &mut Elf,
    sess: &Arc<Mutex<Session>>,
    opts: &cli::Opts,
    current_dir: &Path,
) -> Result<bool, anyhow::Error> {
    let exit = Arc::new(AtomicBool::new(false));
    let sigid = signal_hook::flag::register(signal::SIGINT, exit.clone())?;

    let mut logging_channel = setup_logging_channel(elf.rtt_buffer_address(), sess.clone())?;

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

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut read_buf = [0; 1024];
    let mut defmt_buffer = vec![];
    let mut was_halted = false;
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
                    defmt_buffer.extend_from_slice(&read_buf[..num_bytes_read]);

                    if let Some(value) = decode_and_print_defmt_logs(
                        &mut defmt_buffer,
                        table,
                        elf,
                        current_dir,
                        opts,
                    ) {
                        return value;
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

    signal_hook::low_level::unregister(sigid);
    signal_hook::flag::register_conditional_default(signal::SIGINT, exit.clone())?;

    // Ctrl-C was pressed; stop the microcontroller.
    if exit.load(Ordering::Relaxed) {
        let mut sess = sess.lock().unwrap();
        let mut core = sess.core(0)?;

        core.halt(TIMEOUT)?;
    }

    let halted_due_to_signal = exit.load(Ordering::Relaxed);

    Ok(halted_due_to_signal)
}

fn decode_and_print_defmt_logs(
    defmt_buffer: &mut Vec<u8>,
    table: &defmt_decoder::Table,
    elf: &Elf,
    current_dir: &Path,
    opts: &cli::Opts,
) -> Option<Result<bool, anyhow::Error>> {
    loop {
        match table.decode(&defmt_buffer) {
            Ok((frame, consumed)) => {
                // NOTE(`[]` indexing) all indices in `table` have already been
                // verified to exist in the `locs` map
                let loc = elf
                    .defmt_locations
                    .as_ref()
                    .map(|locs| &locs[&frame.index()]);

                let (mut file, mut line, mut mod_path) = (None, None, None);
                if let Some(loc) = loc {
                    let path = if let Ok(relpath) = loc.file.strip_prefix(&current_dir) {
                        relpath.display().to_string()
                    } else {
                        let dep_path = dep::Path::from_std_path(&loc.file);

                        if opts.shorten_paths {
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
                defmt_decoder::log::log_defmt(&frame, file.as_deref(), line, mod_path.as_deref());

                let num_frames = defmt_buffer.len();
                defmt_buffer.rotate_left(consumed);
                defmt_buffer.truncate(num_frames - consumed);
            }
            Err(defmt_decoder::DecodeError::UnexpectedEof) => break,
            Err(defmt_decoder::DecodeError::Malformed) => {
                log::error!("failed to decode defmt data: {:x?}", defmt_buffer);
                return Some(Err(defmt_decoder::DecodeError::Malformed.into()));
            }
        }
    }
    None
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

/// Print a line to separate different execution stages.
fn print_separator() {
    println!("{}", "─".repeat(80).dimmed());
}
