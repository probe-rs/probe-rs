use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, clap::Parser)]
struct Opt {
    path: PathBuf,

    #[clap(value_parser = parse_address)]
    address: u64,
}

fn parse_address(s: &str) -> Result<u64, std::num::ParseIntError> {
    if let Some(hex_str) = s.strip_prefix("0x") {
        u64::from_str_radix(hex_str, 16)
    } else {
        s.parse::<u64>()
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let opt = Opt::parse();

    tracing::debug!("Hello!");

    let debug_info = probe_rs_debug::DebugInfo::from_file(&opt.path).unwrap();

    let Some(location) = debug_info.get_source_location(opt.address) else {
        eprintln!("Address not found in debug information");
        return;
    };

    println!("Source: {location:?}");
}
