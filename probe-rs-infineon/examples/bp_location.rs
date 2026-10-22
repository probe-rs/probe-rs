//! Debug aid: resolves source breakpoints and addresses of an ELF with probe-rs-debug, offline.
//!
//! ```text
//! cargo run -p probe-rs-infineon --example bp_location -- <elf> <source file> <line> [address]
//! ```

use probe_rs_debug::DebugInfo;
use typed_path::TypedPath;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("--join") {
        let dir = TypedPath::derive("src").to_path_buf().join("demo.c");
        let full = TypedPath::derive(r"C:\work\aurix_tcf_demo").join(&dir);
        println!("{:?} -> {:?} (windows: {})", dir, full, full.is_windows());
        return Ok(());
    }
    let debug_info = DebugInfo::from_file(&args[1])?;
    if let Some(address) = args.get(4) {
        let address = u64::from_str_radix(address.trim_start_matches("0x"), 16)?;
        println!("{address:#010x} -> {:?}", debug_info.get_source_location(address));
    }
    let line: u64 = args[3].parse()?;
    for column in [None, Some(1)] {
        let result = debug_info.get_breakpoint_location(TypedPath::derive(&args[2]), line, column);
        println!("{}:{line} column {column:?} -> {result:?}", args[2]);
    }
    Ok(())
}
