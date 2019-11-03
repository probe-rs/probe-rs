pub mod parser;

use std::io;
use std::fs::{self, DirEntry};
use std::path::{Path,PathBuf};
use slog::Drain;
use cmsis_update::DownloadProgress;
use utils::parse::FromElem;

fn main() {

    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let log = slog::Logger::root(drain, slog::o!());

    let args: Vec<_> = std::env::args().collect();
    let dir = &args[1];
    let device = args[2].to_ascii_lowercase();

    let mut devices = std::collections::HashMap::new();

    visit_dirs(Path::new(&dir), &mut |entry| {
        let package = pdsc::Package::from_path(&entry.path(), &log).map(|p| {
            for (k, v) in p.devices.0 {
                if v.name.to_ascii_lowercase().starts_with(&device) {
                    println!("{:#?}", v);
                }

                if let Some(algorithm) = v.algorithms.iter().next() {
                    devices.insert(v.name.to_ascii_lowercase(), algorithm.clone());
                }
            };
        });
    }).unwrap();

    if let Some(algorithm) = devices.get(&device) {
        println!("{:?}", algorithm);
        
    }
}

struct T {}
impl cmsis_update::DownloadConfig for T {
    fn pack_store(&self) -> PathBuf {
        PathBuf::from(r"./cache/")
    }
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, cb: &mut dyn FnMut(&DirEntry)) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else {
                if path.file_name().unwrap().to_string_lossy().ends_with(".pdsc") {
                    cb(&entry);
                }
            }
        }
    }
    Ok(())
}