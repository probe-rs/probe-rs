//! This example accepts a path to a binary file on the command line, and
//! flashes it to the specified base address.

use std::{num::ParseIntError, path::PathBuf};

use anyhow::{anyhow, Context, Result};
use structopt::StructOpt;

use probe_rs::{
    config::TargetSelector,
    flashing::{self, BinOptions, DownloadOptions, Format},
    Probe, WireProtocol,
};

#[derive(StructOpt)]
struct CLI {
    /// Path to a binary file that will be written to the EEPROM.
    #[structopt(long = "binfile")]
    binfile: PathBuf,
    /// The base address. File contents will be written here.
    #[structopt(long = "address", parse(try_from_str = parse_hex))]
    base_address: u32,
    /// If this is set, then the `keep_unwritten_bytes` option is not set when flashing.
    #[structopt(long = "no-keep-unwritten-bytes")]
    no_keep_unwritten_bytes: bool,
    #[structopt(long = "chip")]
    chip: Option<String>,
    #[structopt(long = "speed")]
    speed: Option<u32>,
    #[structopt(long = "protocol")]
    protocol: Option<String>,
}

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(src.trim_start_matches("0x"), 16)
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    // Parse args
    let matches = CLI::from_args();

    // Open probe
    let mut probe = open_probe(None)?;

    // Select target
    let target_selector = match matches.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    // Set protocol
    let protocol = match matches.protocol {
        Some(protocol) => protocol
            .parse()
            .map_err(|e| anyhow!("Unknown protocol: '{}'", e))?,
        None => WireProtocol::Swd,
    };
    probe
        .select_protocol(protocol)
        .context("Failed to select SWD as the transport protocol")?;

    // Set speed
    if let Some(speed) = matches.speed {
        probe
            .set_speed(speed)
            .context("Failed to set probe speed")?;
    }

    // Attach to target
    let mut session = probe
        .attach(target_selector)
        .context("Failed to attach probe to target")?;

    // Download file
    flashing::download_file_with_options(
        &mut session,
        &matches.binfile,
        Format::Bin(BinOptions {
            base_address: Some(matches.base_address),
            skip: 0,
        }),
        DownloadOptions {
            progress: None,
            keep_unwritten_bytes: !matches.no_keep_unwritten_bytes,
        },
    )?;

    // Reset core
    let mut core = session.core(0).context("Failed to attach to core")?;
    core.reset()?;

    Ok(())
}

fn open_probe(index: Option<usize>) -> Result<Probe> {
    let list = Probe::list_all();

    let device = match index {
        Some(index) => list
            .get(index)
            .ok_or(anyhow!("Probe with specified index not found"))?,
        None => {
            // open the default probe, if only one probe was found
            if list.len() == 1 {
                &list[0]
            } else {
                return Err(anyhow!("No probe found."));
            }
        }
    };

    let probe = device.open().context("Failed to open probe")?;

    Ok(probe)
}
