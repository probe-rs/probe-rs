use probe_rs::{
    config::registry::{Registry, SelectionStrategy},
    coresight::memory::MI,
    probe::MasterProbe,
    session::Session,
    target::info::ChipInfo,
};

use std::num::ParseIntError;
use std::time::Instant;

use pretty_env_logger;
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
}

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(src.trim_start_matches("0x"), 16)
}

fn main() -> Result<(), &'static str> {
    pretty_env_logger::init();

    let matches = CLI::from_args();

    let identifier = &matches.chip;

    let mut probe = open_probe(None)?;

    let strategy = match identifier {
        Some(identifier) => SelectionStrategy::TargetIdentifier(identifier.into()),
        None => SelectionStrategy::ChipInfo(
            ChipInfo::read_from_rom_table(&mut probe)
                .map_err(|_| "Failed to read chip info from ROM table")?,
        ),
    };

    let registry = Registry::from_builtin_families();

    let target = registry
        .get_target(strategy)
        .map_err(|_| "Failed to find target")?;

    let mut session = Session::new(target, probe);

    let data_size_words = matches.size;

    let data_size_bytes = data_size_words * 4;

    let mut rng = rand::thread_rng();

    let mut sample_data = vec![0u32; data_size_words];

    rng.fill(&mut sample_data[..]);

    let write_start = Instant::now();
    session
        .probe
        .write_block32(matches.address, &sample_data)
        .unwrap();

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
    session
        .probe
        .read_block32(matches.address, &mut readback_data)
        .unwrap();
    let read_duration = read_start.elapsed();

    let read_throughput = (data_size_bytes as f32) / read_duration.as_secs_f32();

    println!(
        "Read  {} bytes in {:?} ({:>8.2} bytes/s)",
        data_size_words * 4,
        read_duration,
        read_throughput
    );

    if sample_data != readback_data {
        eprintln!("Verification failed!");
    } else {
        println!("Verification succesful.");
    }

    Ok(())
}

fn open_probe(index: Option<usize>) -> Result<MasterProbe, &'static str> {
    let list = MasterProbe::list_all();

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

    let probe = MasterProbe::from_probe_info(&device).map_err(|_| "Failed to open probe")?;

    Ok(probe)
}
