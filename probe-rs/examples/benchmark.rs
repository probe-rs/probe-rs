use probe_rs::{config::TargetSelector, MemoryInterface, Probe, WireProtocol};

use std::{env, num::ParseIntError, time::SystemTime};
use std::{
    process::Command,
    time::{Duration, Instant, UNIX_EPOCH},
};

use rand::prelude::*;
use structopt::StructOpt;

#[derive(StructOpt)]
struct CLI {
    #[structopt(long = "chip")]
    chip: Option<String>,
    #[structopt(long = "address", parse(try_from_str = parse_hex))]
    address: u32,
    #[structopt(long = "speed")]
    speed: Option<u32>,
    #[structopt(long = "protocol")]
    protocol: Option<String>,
    #[structopt(long = "pr")]
    pr: Option<u64>,
}

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    u32::from_str_radix(src.trim_start_matches("0x"), 16)
}

const SIZE: usize = 0x1000;

fn main() -> Result<(), &'static str> {
    pretty_env_logger::init();

    let matches = CLI::from_args();

    let mut probe = open_probe(None)?;

    let target_selector = match matches.chip.clone() {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    let protocol = match matches.protocol {
        Some(protocol) => protocol.parse().map_err(|_| "Unknown protocol")?,
        None => WireProtocol::Swd,
    };

    let protocol_name = format!("{}", protocol.clone());

    probe
        .select_protocol(protocol)
        .map_err(|_| "Failed to select SWD as the transport protocol")?;

    let protocol_speed = if let Some(speed) = matches.speed {
        let protocol_speed = probe
            .set_speed(speed)
            .map_err(|_| "Failed to set probe speed")?;
        protocol_speed
    } else {
        let protocol_speed = probe
            .set_speed(10000)
            .map_err(|_| "Failed to set probe speed")?;
        protocol_speed
    } as i32;

    if ![100, 1000, 10000, 50000].contains(&protocol_speed) {
        return Err("Speed must be in [100, 1000, 10000, 50000] KHz");
    }

    let probe_name = probe.get_name();

    let mut session = probe
        .attach(target_selector)
        .map_err(|_| "Failed to attach probe to target")?;

    let chip_name = session.target().name.clone();

    let mut core = session.core(0).map_err(|_| "Failed to attach to core")?;

    let data_size_words = SIZE;

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
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let commit_hash = String::from_utf8_lossy(
            &Command::new("git")
                .args(&["rev-parse", "--short", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();

        let commit_name = if Command::new("git")
            .args(&["diff-index", "--quiet", "HEAD", "--"])
            .output()
            .unwrap()
            .status
            .success()
        {
            commit_hash
        } else {
            commit_hash + "-changed"
        };

        let client = reqwest::blocking::Client::new();
        const BASE_URL: &str = "https://perf.probe.rs/add";
        client
            .post(&if let Some(pr) = matches.pr {
                format!("{}?pr={}", BASE_URL, pr)
            } else {
                BASE_URL.to_string()
            })
            .json(&NewLog {
                probe: probe_name,
                chip: chip_name,
                os: env::consts::OS.to_string(),
                protocol: protocol_name,
                protocol_speed: protocol_speed,
                commit_hash: commit_name,
                timestamp: NaiveDateTime::from_timestamp(since_the_epoch as i64, 0),
                kind: "ram".into(),
                read_speed: read_throughput as i32,
                write_speed: write_throughput as i32,
            })
            .send()
            .unwrap();
    }

    Ok(())
}

mod timestamp {
    use chrono::NaiveDateTime;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(date: &NaiveDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(date.timestamp())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = i64::deserialize(deserializer)?;
        Ok(NaiveDateTime::from_timestamp(s, 0))
    }
}

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct NewLog {
    pub probe: String,
    pub chip: String,
    pub os: String,
    pub protocol: String,
    pub protocol_speed: i32,
    pub commit_hash: String,
    #[serde(with = "timestamp")]
    pub timestamp: NaiveDateTime,
    pub kind: String,
    pub read_speed: i32,
    pub write_speed: i32,
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
