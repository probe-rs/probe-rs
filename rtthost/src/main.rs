use probe_rs::probe::list;
use probe_rs::rtt::host::{parse_scan_region, Attach, RttHost};
use probe_rs::rtt::ScanRegion;

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

    let probe_number = match opts.probe {
        ProbeInfo::List => {
            list::list_all_probes(std::io::stdout());
            return Ok(());
        }
        ProbeInfo::Number(i) => i,
    };

    let mut rtthost = RttHost::new(probe_number, opts.chip.as_deref(), Some(opts.scan_region))?;

    if opts.list {
        println!("Up channels:");
        let up_channels = rtthost.up_channel_list()?;
        for chan in up_channels.iter() {
            println!("  {chan}");
        }

        println!("Down channels:");
        let down_channels = rtthost.down_channel_list()?;
        for chan in down_channels.iter() {
            println!("  {chan}");
        }

        return Ok(());
    }

    eprintln!(
        "Found control block at 0x{:08x}",
        rtthost.ctrl_block().unwrap_or_default()
    );

    let attach_behavior = if opts.reset {
        eprintln!("Attaching under reset");
        Attach::UnderReset
    } else {
        Attach::Running
    };

    let spawn_result = rtthost.spawn_channels(attach_behavior)?;
    let stdin = stdin_channel();

    if spawn_result.handle.is_err() {
        bail!("Error spawning RTT thread");
    }

    loop {
        match spawn_result.target_to_host.recv() {
            Ok(data) => {
                stdout().write_all(&data)?;
                stdout().flush()?;
            }
            Err(e) => match spawn_result.handle.unwrap().join().unwrap() {
                Ok(_) => {
                    bail!("Error reading from target: {e}");
                }
                Err(e) => {
                    bail!("Error reading from target: {e:?}");
                }
            },
        }

        if let Ok(data) = stdin.try_recv() {
            spawn_result.host_to_target.send(data)?;
        }
    }
}

fn stdin_channel() -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();

    thread::spawn(move || {
        let mut buf = [0u8; 1024];

        loop {
            match stdin().read(&mut buf[..]) {
                Ok(count) => match tx.send(buf[..count].to_vec()) {
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("Error sending to target: {err}");
                        break;
                    }
                },
                Err(err) => {
                    eprintln!("Error reading from stdin, input disabled: {err}");
                    break;
                }
            }
        }
    });

    rx
}
