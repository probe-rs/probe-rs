use std::{
    num::ParseIntError,
    time::{Duration, Instant},
};

use anyhow::Context;
use probe_rs::{MemoryInterface, config::Registry, probe::list::Lister};

use crate::util::common_options::LoadedProbeOptions;
use crate::util::common_options::ProbeOptions;

const PROBE_SPEEDS: [u32; 10] = [320, 640, 960, 3200, 6400, 9600, 32000, 64000, 96000, 320000];
const TEST_SIZES: [usize; 5] = [1, 8, 32, 512, 8192];

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    /// Start address for the benchmark test.
    ///
    /// Should be located in RAM.
    #[clap(long = "address", value_parser= parse_hex)]
    address: u64,

    /// Minimum speed for the debug probe.
    ///
    /// Some probes will panic if you request a speed lower than they support.
    /// This option will set a lower-bound for the speeds that will be tested
    #[clap(long = "min-speed", value_parser= parse_int, default_value="0")]
    min_speed: u32,

    /// Maximum speed for the debug probe.
    ///
    /// Some probes will panic if you request a speed higher than they support.
    /// Data may also become corrupt at higher speeds due to cabling issues.
    /// This option will set a upper-bound for the speeds that will be tested
    #[clap(long = "max-speed", value_parser= parse_int, default_value="0")]
    max_speed: u32,

    /// Word size for read/write accesses.
    ///
    /// Set the read/write word size to 8/32/64bits.
    /// Note: not all chips/probes support all sizes. 32bit is a safe default
    #[clap(long = "word-size", value_parser= parse_int, default_value="32")]
    word_size: u32,

    /// Number of times to run each test
    ///
    /// Especially for short tests (high speed + low size) there will be some error
    /// in measurement. By running multiple iterations of each test we should be able to
    /// both reduce the amount of jitter, and also quantify it (via standard deviation calcs)
    #[clap(long = "iterations", value_parser= parse_usize, default_value="5")]
    iterations: usize,
}

fn parse_usize(src: &str) -> Result<usize, ParseIntError> {
    src.parse::<usize>()
}

fn parse_int(src: &str) -> Result<u32, ParseIntError> {
    src.parse::<u32>()
}

fn parse_hex(src: &str) -> Result<u64, ParseIntError> {
    u64::from_str_radix(src.trim_start_matches("0x"), 16)
}

#[derive(Debug)]
/// Provide different data arrays for each read/write stride size
enum DataType {
    U8(Vec<u8>, Vec<u8>),
    U32(Vec<u32>, Vec<u32>),
    U64(Vec<u64>, Vec<u64>),
}

#[derive(Debug)]
/// Configuration and results for a test run
struct TestData {
    address: u64,
    word_qty: usize,
    pub data_type: DataType,
}

impl Cmd {
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let speed = self.common.speed;
        let common_options = self.common.load(registry)?;
        let mut max_speed = self.max_speed;
        let mut speeds = vec![];
        // if no max-speed specified, assume the user just wants to use a single speed (as per other cli cmds)
        if self.max_speed == 0 {
            max_speed = speed.unwrap_or(3000);
            speeds.push(max_speed);
        } else {
            speeds.extend_from_slice(&PROBE_SPEEDS);
        };
        // if we can't print basic info, we're probably not going to succeed with testing so bubble up the error
        Cmd::print_info(&common_options, lister)?;

        for speed in speeds
            .iter()
            .filter(|speed| (self.min_speed..=max_speed).contains(*speed))
        {
            for size in TEST_SIZES {
                let res = Cmd::benchmark(
                    &common_options,
                    lister,
                    *speed,
                    size,
                    self.address,
                    self.word_size,
                    self.iterations,
                );
                if let Err(e) = res {
                    println!(
                        "Test failed for speed {} size {} word_size {}bit - {}",
                        speed, size, self.word_size, e
                    )
                }
            }
        }

        Ok(())
    }

    /// Print probe and target info
    fn print_info(common_options: &LoadedProbeOptions, lister: &Lister) -> anyhow::Result<()> {
        let probe = common_options.attach_probe(lister)?;
        let protocol_name = probe
            .protocol()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "not specified".to_string());

        let target = common_options.get_target_selector()?;
        let probe_name = probe.get_name();
        let session = common_options.attach_session(probe, target)?;
        let target_name = session.target().name.clone();
        println!(
            "Probe: Probe type {}, debug interface {}, target chip {}\n",
            probe_name, protocol_name, target_name
        );
        Ok(())
    }

    /// Run a specific benchmark
    fn benchmark(
        common_options: &LoadedProbeOptions,
        lister: &Lister,
        speed: u32,
        size: usize,
        address: u64,
        word_size: u32,
        iterations: usize,
    ) -> Result<(), anyhow::Error> {
        let mut probe = common_options.attach_probe(lister)?;
        let target = common_options.get_target_selector()?;
        if probe.set_speed(speed).is_ok() {
            let mut session = common_options.attach_session(probe, target)?;
            let mut test = TestData::new(address, word_size, size);
            println!(
                "Test: Speed {}, Word size {}bit, Data length {} bytes, Number of iterations {}",
                speed,
                word_size,
                test.data_type.size() * size,
                iterations
            );
            let mut core = session.core(0).context("Failed to attach to core")?;
            core.halt(Duration::from_millis(100))
                .context("Halting failed")?;

            let mut read_results = Vec::<f64>::with_capacity(iterations);
            let mut write_results = Vec::<f64>::with_capacity(iterations);
            'inner: for _ in 0..iterations {
                let write_throughput = test.block_write(&mut core)?;
                let read_throughput = test.block_read(&mut core)?;
                let verify_success = test.block_verify();
                if verify_success {
                    read_results.push(read_throughput);
                    write_results.push(write_throughput);
                } else {
                    eprintln!("Verification failed.");
                    break 'inner;
                }
            }
            println!(
                "Results: Read: {:.2} bytes/s Std Dev {:.2}, Write: {:.2} bytes/s Std Dev {:.2}",
                mean(&read_results).expect("invalid mean"),
                std_deviation(&read_results).expect("invalid std deviation"),
                mean(&write_results).expect("invalid mean"),
                std_deviation(&write_results).expect("invalid std deviation")
            );
            if read_results.len() != iterations || write_results.len() != iterations {
                println!(
                    "Warning: {} reads and {} writes successful (out of {} iterations)",
                    read_results.len(),
                    write_results.len(),
                    iterations
                )
            }
            // Insert another blank line to visually seperate results
            println!();
        } else {
            println!("failed to set speed {}", speed);
        }
        Ok(())
    }
}

impl DataType {
    pub fn new(word_size: u32) -> DataType {
        match word_size {
            8 => DataType::U8(Vec::new(), Vec::new()),
            32 => DataType::U32(Vec::new(), Vec::new()),
            64 => DataType::U64(Vec::new(), Vec::new()),
            _ => panic!("Invalid word size"),
        }
    }

    pub fn size(&self) -> usize {
        match self {
            DataType::U8(_, _) => 1,
            DataType::U32(_, _) => 4,
            DataType::U64(_, _) => 8,
        }
    }

    pub fn fill_data(&mut self, data_size_words: usize) {
        let mut rng = fastrand::Rng::new();
        match self {
            DataType::U8(test_data, read_data) => {
                *test_data = vec![0u8; data_size_words];
                *read_data = vec![0u8; data_size_words];
                rng.fill(&mut test_data[..]);
            }
            DataType::U32(test_data, read_data) => {
                *test_data = vec![0u32; data_size_words];
                *read_data = vec![0u32; data_size_words];
                for out in test_data.iter_mut() {
                    *out = rng.u32(..);
                }
            }
            DataType::U64(test_data, read_data) => {
                *test_data = vec![0u64; data_size_words];
                *read_data = vec![0u64; data_size_words];
                for out in test_data.iter_mut() {
                    *out = rng.u64(..);
                }
            }
        }
    }

    pub fn compare_data(&self) -> Option<usize> {
        fn compare_data_inner<T: PartialEq>(sample_data: &[T], read_data: &[T]) -> Option<usize> {
            let mismatch = sample_data
                .iter()
                .zip(read_data.iter())
                .position(|(sample, readback)| sample != readback);
            mismatch
        }
        match self {
            DataType::U8(sample_data, read_data) => compare_data_inner(sample_data, read_data),
            DataType::U32(sample_data, read_data) => compare_data_inner(sample_data, read_data),
            DataType::U64(sample_data, read_data) => compare_data_inner(sample_data, read_data),
        }
    }

    pub fn data_at_pos(&self, offset: usize) -> (u64, u64) {
        match self {
            DataType::U8(sample_data, read_data) => {
                (sample_data[offset].into(), read_data[offset].into())
            }
            DataType::U32(sample_data, read_data) => {
                (sample_data[offset].into(), read_data[offset].into())
            }
            DataType::U64(sample_data, read_data) => (sample_data[offset], read_data[offset]),
        }
    }
}

impl TestData {
    fn new(address: u64, word_size: u32, word_qty: usize) -> TestData {
        let mut data_type = DataType::new(word_size);
        data_type.fill_data(word_qty);

        TestData {
            address,
            data_type,
            word_qty,
        }
    }

    fn block_verify(&self) -> bool {
        if let Some(mismatch) = self.data_type.compare_data() {
            let (sample_data, readback_data) = self.data_type.data_at_pos(mismatch);
            eprintln!(
                "Readback data differs at address {:08x}: expected word {:08x}, got word {:08x}",
                self.address + mismatch as u64,
                sample_data,
                readback_data
            );
            false
        } else {
            true
        }
    }

    /// Read the requested block of data. Return data throughput, or error
    fn block_read(&mut self, core: &mut probe_rs::Core) -> Result<f64, anyhow::Error> {
        let read_start = Instant::now();
        match &mut self.data_type {
            DataType::U8(_, readback_data) => core
                .read_8(self.address, readback_data)
                .expect("Reading the sample data failed"),
            DataType::U32(_, readback_data) => core
                .read_32(self.address, readback_data)
                .expect("Reading the sample data failed"),
            DataType::U64(_, readback_data) => core
                .read_64(self.address, readback_data)
                .expect("Reading the sample data failed"),
        }
        let read_duration = read_start.elapsed();
        let data_size_bytes = self.data_type.size() * self.word_qty;
        let read_throughput = (data_size_bytes as f64) / read_duration.as_secs_f64();

        Ok(read_throughput)
    }

    /// Write the requested block of data. Return data throughput, or error
    fn block_write(&mut self, core: &mut probe_rs::Core) -> Result<f64, anyhow::Error> {
        let write_start = Instant::now();
        match &self.data_type {
            DataType::U8(test_data, _) => core
                .write_8(self.address, test_data)
                .context("Writing the sample data failed")?,
            DataType::U32(test_data, _) => core
                .write_32(self.address, test_data)
                .context("Writing the sample data failed")?,
            DataType::U64(test_data, _) => core
                .write_64(self.address, test_data)
                .context("Writing the sample data failed")?,
        }
        let write_duration = write_start.elapsed();
        let data_size_bytes = self.data_type.size() * self.word_qty;
        let write_throughput = (data_size_bytes as f64) / write_duration.as_secs_f64();

        Ok(write_throughput)
    }
}

/// Calculate arithmetic mean for data
fn mean(data: &[f64]) -> Option<f64> {
    let sum = data.iter().sum::<f64>();
    let count = data.len() as f64;

    match count {
        positive if positive > 0.0 => Some(sum / count),
        _ => None,
    }
}

/// Calculate standard deviation across data
fn std_deviation(data: &[f64]) -> Option<f64> {
    match (mean(data), data.len()) {
        (Some(data_mean), count) if count > 0 => {
            let variance = data
                .iter()
                .map(|value| {
                    let diff = data_mean - *value;

                    diff * diff
                })
                .sum::<f64>()
                / count as f64;

            Some(variance.sqrt())
        }
        _ => None,
    }
}
