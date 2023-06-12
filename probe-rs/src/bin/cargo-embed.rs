use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    args.insert(0, "cargo-embed".into());
    let err = Command::new("probe-rs").args(&args).exec();
    eprintln!("Error: {}", err);
    std::process::exit(99);
}
