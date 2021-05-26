mod backtrace;
mod canary;
mod cli;
mod cortexm;
mod dep;
mod elf;
mod registers;
mod stacked;
mod target_info;

use std::{
    fs,
    io::{self, Write as _},
    path::Path,
    process,
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail};
use colored::Colorize as _;
use probe_rs::{
    flashing::{self, Format},
    DebugProbeInfo, Probe, Session,
};
use probe_rs_rtt::{Rtt, ScanRegion, UpChannel};
use signal_hook::consts::signal;

use crate::{backtrace::Outcome, elf::Elf, target_info::TargetInfo};

const SIGABRT: i32 = 134;
const TIMEOUT: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    cli::handle_arguments().map(|code| process::exit(code))
}

fn run_target_program(elf_path: &Path, chip: &str, opts: &cli::Opts) -> anyhow::Result<i32> {
    if !elf_path.exists() {
        return Err(anyhow!(
            "can't find ELF file at `{}`; are you sure you got the right path?",
            elf_path.display()
        ));
    }

    let elf_bytes = fs::read(elf_path)?;
    let mut elf = Elf::parse(&elf_bytes)?;

    let target_info = TargetInfo::new(chip, &elf)?;

    let mut probe = open_probe(opts)?;

    if let Some(speed) = opts.speed {
        probe.set_speed(speed)?;
    }

    let target = target_info.target.clone();
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
        let size = elf.program_size();
        log::info!("flashing program ({:.02} KiB)", size as f64 / 1024.0);
        flashing::download_file(&mut sess, elf_path, Format::Elf)?;
        log::info!("success!");
    }

    let canary = canary::place(&mut sess, &target_info, &elf)?;

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

    print_separator();

    let canary_touched = canary::touched(canary, &mut core, &elf)?;
    let halted_due_to_signal = exit.load(Ordering::Relaxed);
    let backtrace_settings = backtrace::Settings {
        current_dir: &current_dir,
        max_backtrace_len: opts.max_backtrace_len,
        // TODO any other cases in which we should force a backtrace?
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

fn open_probe(opts: &cli::Opts) -> Result<Probe, anyhow::Error> {
    let all_probes = Probe::list_all();
    let filtered_probes = if let Some(probe_opt) = opts.probe.as_deref() {
        let selector = probe_opt.parse()?;
        probes_filter(&all_probes, &selector)
    } else {
        all_probes
    };

    if filtered_probes.is_empty() {
        bail!("no probe was found")
    }

    log::debug!("found {} probes", filtered_probes.len());

    if filtered_probes.len() > 1 {
        let _ = print_probes(filtered_probes);
        bail!("more than one probe found; use --probe to specify which one to use");
    }

    let probe = filtered_probes[0].open()?;
    log::debug!("opened probe");
    Ok(probe)
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

/// Print a line to separate different execution stages.
fn print_separator() {
    println!("{}", "─".repeat(80).dimmed());
}
