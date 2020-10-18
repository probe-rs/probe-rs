use probe_rs::{config::TargetSelector, MemoryInterface, Probe, WireProtocol};

use std::num::ParseIntError;
use std::time::{Duration, Instant};

use rand::prelude::*;
use structopt::StructOpt;

#[derive(StructOpt)]
struct CLI {
    #[structopt(long = "chip")]
    chip: Option<String>,
    #[structopt(long = "address", parse(try_from_str = parse_hex))]
    address: u32,
    #[structopt(long = "size")]
    size: usize,
    #[structopt(long = "speed")]
    speed: Option<u32>,
    #[structopt(long = "protocol")]
    protocol: Option<String>,
}

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(src.trim_start_matches("0x"), 16)
}

fn main() -> Result<(), &'static str> {
    pretty_env_logger::init();

    let matches = CLI::from_args();

    let mut probe = open_probe(None)?;

    let target_selector = match matches.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    let protocol = match matches.protocol {
        Some(protocol) => protocol.parse().map_err(|_| "Unknown protocol")?,
        None => WireProtocol::Swd,
    };

    probe
        .select_protocol(protocol)
        .map_err(|_| "Failed to select SWD as the transport protocol")?;

    if let Some(speed) = matches.speed {
        probe
            .set_speed(speed)
            .map_err(|_| "Failed to set probe speed")?;
    }

    let mut session = probe
        .attach(target_selector)
        .map_err(|_| "Failed to attach probe to target")?;
    let mut core = session.core(0).map_err(|_| "Failed to attach to core")?;

    let data_size_words = matches.size;

    let data_size_bytes = data_size_words * 4;

    let mut rng = rand::thread_rng();

    let mut sample_data = vec![0u32; data_size_words];

    rng.fill(&mut sample_data[..]);

    core.halt(Duration::from_millis(100))
        .expect("Halting failed");

    let write_start = Instant::now();
    core.write_32(matches.address, &sample_data)
        .expect("Writing the sample data failed");

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

    if sample_data != readback_data {
        let mismatch = sample_data
            .iter()
            .zip(readback_data.iter())
            .position(|(sample, readback)| sample != readback);

        eprintln!("Verification failed!");

        if let Some(mismatch) = mismatch {
            eprintln!(
                "Readback data differs at address {:08x}: expected word {:08x}, got word {:08x}",
                matches.address, sample_data[mismatch], readback_data[mismatch]
            );
        }
    } else {
        println!("Verification succesful.");
    }

    Ok(())
}

fn open_probe(index: Option<usize>) -> Result<Probe, &'static str> {
    let list = Probe::list_all();

    let device = match index {
        Some(index) => list
            .get(index)
            .ok_or("Probe with specified index not found")?,
        None => {
            // open the default probe, if only one probe was found
            if list.len() == 1 {
                &list[0]
            } else {
                return Err("No probe found.");
            }
        }
    };

    let probe = device.open().map_err(|e| {
        println!("{}", e);
        "Failed to open probe"
    })?;

    Ok(probe)
}
