use std::{
    env,
    num::ParseIntError,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use probe_rs::MemoryInterface;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::util::common_options::ProbeOptions;

const SIZE: usize = 0x1000;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    /// Start address for the benchmark test.
    ///
    /// Should be located in RAM.
    #[clap(long = "address", value_parser= parse_hex)]
    address: u64,
    #[clap(long = "pr")]
    pr: Option<u64>,

    #[clap(long)]
    upload: bool,
}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let common_options = self.common.load()?;
        let probe = common_options.attach_probe()?;

        let protocol_name = probe
            .protocol()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "Unknown protocol".to_string());

        let protocol_speed = probe.speed_khz() as i32;

        let target = common_options.get_target_selector()?;
        let probe_name = probe.get_name();
        let mut session = common_options.attach_session(probe, target)?;

        let target_name = session.target().name.clone();

        let mut core = session.core(0).context("Failed to attach to core")?;

        let data_size_words = SIZE;

        let data_size_bytes = data_size_words * 4;

        let mut rng = rand::thread_rng();

        let mut sample_data = vec![0u32; data_size_words];

        rng.fill(&mut sample_data[..]);

        core.halt(Duration::from_millis(100))
            .context("Halting failed")?;

        let write_start = Instant::now();
        core.write_32(self.address, &sample_data)
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
        core.read_32(self.address, &mut readback_data)
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
                    self.address, sample_data[mismatch], readback_data[mismatch]
                );
            }

            Ok(())
        } else {
            println!("Verification succesful.");

            if self.upload {
                let start = SystemTime::now();
                let since_the_epoch = start
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_secs();

                let commit_hash = String::from_utf8_lossy(
                    &Command::new("git")
                        .args(["rev-parse", "--short", "HEAD"])
                        .output()
                        .unwrap()
                        .stdout,
                )
                .trim()
                .to_string();

                let commit_name = if Command::new("git")
                    .args(["diff-index", "--quiet", "HEAD", "--"])
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
                    .post(if let Some(pr) = self.pr {
                        format!("{BASE_URL}?pr={pr}")
                    } else {
                        BASE_URL.to_string()
                    })
                    .json(&NewLog {
                        probe: probe_name,
                        chip: target_name,
                        os: env::consts::OS.to_string(),
                        protocol: protocol_name,
                        protocol_speed,
                        commit_hash: commit_name,
                        timestamp: OffsetDateTime::from_unix_timestamp(since_the_epoch as i64)
                            .unwrap(),
                        kind: "ram".into(),
                        read_speed: read_throughput as i32,
                        write_speed: write_throughput as i32,
                    })
                    .send()
                    .with_context(|| format!("Failed to upload results to {BASE_URL}"))?;
            }

            Ok(())
        }
    }
}

fn parse_hex(src: &str) -> Result<u64, ParseIntError> {
    u64::from_str_radix(src.trim_start_matches("0x"), 16)
}

#[derive(Serialize, Deserialize)]
pub struct NewLog {
    pub probe: String,
    pub chip: String,
    pub os: String,
    pub protocol: String,
    pub protocol_speed: i32,
    pub commit_hash: String,
    #[serde(with = "timestamp")]
    pub timestamp: OffsetDateTime,
    pub kind: String,
    pub read_speed: i32,
    pub write_speed: i32,
}

mod timestamp {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use time::OffsetDateTime;

    pub fn serialize<S>(date: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(date.unix_timestamp())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = i64::deserialize(deserializer)?;
        Ok(OffsetDateTime::from_unix_timestamp(s).unwrap())
    }
}
