pub mod parser;
pub mod flash_device;
pub mod algorithm_binary;

use std::io;
use std::fs::{self, DirEntry};
use std::path::{Path};
use slog::Drain;
use utils::parse::FromElem;
use probe_rs::probe::flash::RamRegion;

fn main() {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let log = slog::Logger::root(drain, slog::o!());

    let args: Vec<_> = std::env::args().collect();
    let dir = &std::path::Path::new(&args[1]);
    let device = args[2].to_ascii_lowercase();

    visit_dirs(Path::new(&dir), &mut |entry| {
        pdsc::Package::from_path(&entry.path(), &log).map(|p| {
            for (k, v) in p.devices.0 {
                if v.name.to_ascii_lowercase().starts_with(&device) {
                    println!("{:#?}", v);
                    for algorithm in v.algorithms.iter() {
                        if algorithm.default {
                            let algo = crate::parser::extract_flash_algo(
                                dir
                                    .join(algorithm.file_name
                                        .as_path()
                                        .to_string_lossy()
                                        .replace("\\", "/")
                                    )
                                    .as_path(),
                                RamRegion {
                                    is_boot_memory: true,
                                    is_testable: true,
                                    range: 0x1000_0000..0x1000_4000,
                                })
                            .unwrap();

                            algo.write_to_file("test.yaml");
                            break;
                        }
                    }
                }
            };
        }).unwrap();
    }).unwrap();
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, cb: &mut dyn FnMut(&DirEntry)) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else if path.file_name().unwrap().to_string_lossy().ends_with(".pdsc") {
                cb(&entry);
            }
        }
    }
    Ok(())
}
