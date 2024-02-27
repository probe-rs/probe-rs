use probe_rs::rtt::{Channels, Rtt, RttChannel, ScanRegion};
use probe_rs::{config::TargetSelector, probe::DebugProbeInfo};
use probe_rs::{probe::list::Lister, Permissions};

use anyhow::{bail, Result};
use clap::Parser;
use std::io::prelude::*;
use std::io::{stdin, stdout};
use std::sync::mpsc::{channel, Receiver};
use std::thread;

#[derive(Debug, PartialEq, Eq, Clone)]
enum ProbeInfo {
    Number(usize),
    List,
}

impl std::str::FromStr for ProbeInfo {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<ProbeInfo, &'static str> {
        if s == "list" {
            Ok(ProbeInfo::List)
        } else if let Ok(n) = s.parse::<usize>() {
            Ok(ProbeInfo::Number(n))
        } else {
            Err("Invalid probe number.")
        }
    }
}

fn parse_scan_region(
    mut src: &str,
) -> Result<ScanRegion, Box<dyn std::error::Error + Send + Sync + 'static>> {
    src = src.trim();
    if src.is_empty() {
        return Ok(ScanRegion::Ram);
    }

    let parts = src
        .split("..")
        .map(|p| {
            if p.starts_with("0x") || p.starts_with("0X") {
                u32::from_str_radix(&p[2..], 16)
            } else {
                p.parse()
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    match *parts.as_slice() {
        [addr] => Ok(ScanRegion::Exact(addr)),
        [start, end] => Ok(ScanRegion::Range(start..end)),
        _ => Err("Invalid range: multiple '..'s".into()),
    }
}

#[derive(Debug, clap::Parser)]
#[clap(
    name = "rtthost",
    about = "Host program for debugging microcontrollers using the RTT (real-time transfer) protocol.",
    version = clap::crate_version!(),
)]
struct Opts {
    #[clap(
        short,
        long,
        default_value = "0",
        help = "Specify probe number or 'list' to list probes."
    )]
    probe: ProbeInfo,

    #[clap(
        short,
        long,
        help = "Target chip type. Leave unspecified to auto-detect."
    )]
    chip: Option<String>,

    #[clap(short, long, help = "List RTT channels and exit.")]
    list: bool,

    #[clap(
        short,
        long,
        help = "Number of up channel to output. Defaults to 0 if it exists."
    )]
    up: Option<usize>,

    #[clap(
        short,
        long,
        help = "Number of down channel for keyboard input. Defaults to 0 if it exists."
    )]
    down: Option<usize>,

    #[clap(short, long, help = "Reset the target after RTT session was opened")]
    reset: bool,

    #[clap(
        long,
        default_value="",
        value_parser = parse_scan_region,
        help = "Memory region to scan for control block. You can specify either an exact starting address '0x1000' or a range such as '0x0000..0x1000'. Both decimal and hex are accepted.")]
    scan_region: ScanRegion,
}

fn main() -> Result<()> {
    pretty_env_logger::init();
    let opts = Opts::parse();

    let lister = Lister::new();

    let probes = lister.list_all();

    if probes.is_empty() {
        bail!("No debug probes available. Make sure your probe is plugged in, supported and up-to-date.");
    }

    let probe_number = match opts.probe {
        ProbeInfo::List => {
            list_probes(std::io::stdout(), &probes);
            return Ok(());
        }
        ProbeInfo::Number(i) => i,
    };

    if probe_number >= probes.len() {
        list_probes(std::io::stderr(), &probes);
        bail!("Probe {probe_number} does not exist.");
    }

    let probe = match probes[probe_number].open() {
        Ok(probe) => probe,
        Err(err) => {
            bail!("Error opening probe: {err}");
        }
    };

    let target_selector = TargetSelector::from(opts.chip.as_deref());

    let mut session = match probe.attach(target_selector, Permissions::default()) {
        Ok(session) => session,
        Err(err) => {
            let mut err_str = format!("Error creating debug session: {err}");

            if opts.chip.is_none() {
                if let probe_rs::Error::ChipNotFound(_) = err {
                    err_str
                        .push_str("\nHint: Use '--chip' to specify the target chip type manually");
                }
            }

            bail!("{err}");
        }
    };

    let memory_map = session.target().memory_map.clone();

    let mut core = match session.core(0) {
        Ok(core) => core,
        Err(err) => {
            bail!("Error attaching to core # 0 {err}");
        }
    };

    eprintln!("Attaching to RTT...");

    let mut rtt = match Rtt::attach_region(&mut core, &memory_map, &opts.scan_region) {
        Ok(rtt) => rtt,
        Err(err) => {
            bail!("Error attaching to RTT: {err}");
        }
    };

    if opts.list {
        println!("Up channels:");
        list_channels(rtt.up_channels());

        println!("Down channels:");
        list_channels(rtt.down_channels());

        return Ok(());
    }

    let up_channel = if let Some(up) = opts.up {
        let chan = rtt.up_channels().take(up);

        if chan.is_none() {
            bail!("Error: up channel {up} does not exist.");
        }

        chan
    } else {
        rtt.up_channels().take(0)
    };

    let down_channel = if let Some(down) = opts.down {
        let chan = rtt.down_channels().take(down);

        if chan.is_none() {
            bail!("Error: up channel {down} does not exist.");
        }

        chan
    } else {
        rtt.down_channels().take(0)
    };

    let stdin = down_channel.as_ref().map(|_| stdin_channel());

    eprintln!("Found control block at 0x{:08x}", rtt.ptr());

    let mut up_buf = [0u8; 1024];
    let mut down_buf = vec![];

    if opts.reset {
        core.reset()?;
    }

    loop {
        if let Some(up_channel) = up_channel.as_ref() {
            let count = match up_channel.read(&mut core, up_buf.as_mut()) {
                Ok(count) => count,
                Err(err) => {
                    bail!("\nError reading from RTT: {err}");
                }
            };

            match stdout().write_all(&up_buf[..count]) {
                Ok(_) => {
                    stdout().flush().ok();
                }
                Err(err) => {
                    bail!("Error writing to stdout: {err}");
                }
            }
        }

        if let (Some(down_channel), Some(stdin)) = (down_channel.as_ref(), &stdin) {
            if let Ok(bytes) = stdin.try_recv() {
                down_buf.extend_from_slice(bytes.as_slice());
            }

            if !down_buf.is_empty() {
                let count = match down_channel.write(&mut core, down_buf.as_mut()) {
                    Ok(count) => count,
                    Err(err) => {
                        bail!("\nError writing to RTT: {err}");
                    }
                };

                if count > 0 {
                    down_buf.drain(..count);
                }
            }
        }
    }
}

fn list_probes(mut stream: impl std::io::Write, probes: &[DebugProbeInfo]) {
    writeln!(stream, "Available probes:").unwrap();

    for (i, probe) in probes.iter().enumerate() {
        writeln!(
            stream,
            "  {}: {} {}",
            i,
            probe.identifier,
            probe
                .serial_number
                .as_deref()
                .unwrap_or("(no serial number)")
        )
        .unwrap();
    }
}

fn list_channels(channels: &Channels<impl RttChannel>) {
    if channels.is_empty() {
        println!("  (none)");
        return;
    }

    for chan in channels.iter() {
        println!(
            "  {}: {} (buffer size {})",
            chan.number(),
            chan.name().unwrap_or("(no name)"),
            chan.buffer_size(),
        );
    }
}

fn stdin_channel() -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();

    thread::spawn(move || {
        let mut buf = [0u8; 1024];

        loop {
            match stdin().read(&mut buf[..]) {
                Ok(count) => {
                    tx.send(buf[..count].to_vec()).unwrap();
                }
                Err(err) => {
                    eprintln!("Error reading from stdin, input disabled: {err}");
                    break;
                }
            }
        }
    });

    rx
}
