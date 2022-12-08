use probe_rs::{config::TargetSelector, MemoryInterface, Permissions, Probe, WireProtocol};

use clap::Parser;
use std::num::ParseIntError;
use std::time::{Duration, Instant};

use rand::prelude::*;

use anyhow::{anyhow, Context, Result};

#[derive(clap::Parser)]
struct Cli {
    #[clap(long = "chip")]
    chip: Option<String>,
    #[clap(long = "address", value_parser = parse_hex)]
    address: u64,
    #[clap(long = "size")]
    size: usize,
    #[clap(long = "speed")]
    speed: Option<u32>,
    #[clap(long = "protocol")]
    protocol: Option<String>,
}

fn parse_hex(src: &str) -> Result<u64, ParseIntError> {
    u64::from_str_radix(src.trim_start_matches("0x"), 16)
}

fn main() -> Result<()> {
    pretty_env_logger::init();

    let matches = Cli::parse();

    let mut probe = open_probe(None)?;

    let target_selector = match matches.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    let protocol = match matches.protocol {
        Some(protocol) => protocol
            .parse()
            .map_err(|e| anyhow!("Unknown protocol: '{}'", e))?,
        None => WireProtocol::Swd,
    };

    probe
        .select_protocol(protocol)
        .context("Failed to select SWD as the transport protocol")?;

    if let Some(speed) = matches.speed {
        probe
            .set_speed(speed)
            .context("Failed to set probe speed")?;
    }

    let mut session = probe
        .attach(target_selector, Permissions::default())
        .context("Failed to attach probe to target")?;
    let mut core = session.core(0).context("Failed to attach to core")?;

    let data_size_words = matches.size;

    let data_size_bytes = data_size_words * 4;

    let mut rng = rand::thread_rng();

    let mut sample_data = vec![0u32; data_size_words];

    rng.fill(&mut sample_data[..]);

    core.halt(Duration::from_millis(100))
        .expect("Halting failed");

    let write_start = Instant::now();
    core.write_32(matches.address, &sample_data)
        .context("Writing the sample data failed")?;

    let write_duration = write_start.elapsed();

    let write_throughput = (data_size_bytes as f32) / write_duration.as_secs_f32();

    println!(
        "Wrote {} bytes in {:?} ({:>8.2} bytes/s)",
        data_size_words * 4,
        write_duration,
        write_throughput
    );

    // read back data

    let mut readback_data = vec![0u32; data_size_words];

    let read_start = Instant::now();
    core.read_32(matches.address, &mut readback_data)
        .expect("Reading the sample data failed");
    let read_duration = read_start.elapsed();

    let read_throughput = (data_size_bytes as f32) / read_duration.as_secs_f32();

    println!(
        "Read  {} bytes in {:?} ({:>8.2} bytes/s)",
        data_size_words * 4,
        read_duration,
        read_throughput
    );

    let max_error_count = 10;

    let mut error_count = 0;

    for (index, (sample_data, readback_data)) in
        sample_data.iter().zip(readback_data.iter()).enumerate()
    {
        if sample_data != readback_data {
            let mismatch_address = matches.address + index as u64 * 4;

            eprintln!(
                "Readback data differs at address {:08x}: expected word {:08x}, got word {:08x}",
                mismatch_address, sample_data, readback_data
            );

            error_count += 1;
        }

        if error_count >= max_error_count {
            break;
        }
    }

    if error_count > 0 {
        println!(
            "First element: {:08x} ?= {:08x}",
            sample_data[0], readback_data[0]
        );
        println!(
            "Last element: {:08x} ?= {:08x}",
            sample_data[sample_data.len() - 1],
            readback_data[readback_data.len() - 1]
        );
        eprintln!("Verification failed!");
    } else {
        println!("Verification succesful.");
    }

    Ok(())
}

fn open_probe(index: Option<usize>) -> Result<Probe> {
    let list = Probe::list_all();

    let device = match index {
        Some(index) => list
            .get(index)
            .ok_or_else(|| anyhow!("Probe with specified index not found"))?,
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
