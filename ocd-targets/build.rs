use std::env;
use std::fs::{
    File,
    read_dir,
    read_to_string,
};
use std::io::{
    Write,
    self,
};
use std::path::Path;

use ocd::target::Target;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("targets.rs");
    let mut f = File::create(&dest_path).unwrap();

    let mut files = vec![];
    visit_dirs(Path::new("targets"), &mut files).unwrap();

    let mut names = vec![];
    let mut indices = vec![];
    let mut target_files = vec![];

    for file in files {
        let string = read_to_string(&file)
            .expect("Chip definition file could not be read. This is a bug. Please report it.");
        match Target::new(&string) {
            Ok(target) => {
                target_files.push("/".to_string() + &file);
                for name in target.names {
                    names.push(name.to_ascii_lowercase());
                    indices.push(target_files.len() - 1);
                }
            },
            Err(e) => {
                log::error!("Failed to parse file {}.", string);
                log::error!("{:?}.", e);
            }
        }
    }

    let stream: String = format!("{}", quote::quote! {
    // START QUOTE
        use std::collections::HashMap;

        use ocd::{
            target::{
                Target,
                TargetSelectionError,
                identify_target,
            },
            collection
        };

        lazy_static::lazy_static! {
            static ref NAMES: HashMap<&'static str, usize> = vec![
                #((#names, #indices),)*
            ].into_iter().collect();

            static ref TARGETS: Vec<&'static str> = vec! [
                #(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), #target_files)),)*
            ];
        }

        pub fn get_built_in_target(name: impl AsRef<str>) -> Result<Target, TargetSelectionError> {
            let name = name.as_ref().to_string().to_ascii_lowercase();
            NAMES
                .get(&name[..])
                .ok_or(TargetSelectionError::TargetNotFound(name))
                .and_then(|i| { Target::new(TARGETS[*i]).map_err(From::from) })
        }

        pub fn select_target(name: Option<&str>) -> Result<Target, TargetSelectionError> {
            match name {
                Some(name) => {
                    let target = match collection::get_target(name) {
                        Some(target) => Some(target),
                        None => None,
                    };
                    match target {
                        Some(target) => Ok(target),
                        None => get_built_in_target(name),
                    }
                },
                None => identify_target(),
            }
        }
    // END QUOTE
    });

    f.write_all(stream.as_bytes()).expect("Writing build.rs output failed.");
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, targets: &mut Vec<String>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, targets)?;
            } else {
                targets.push(format!("{}", path.to_str().unwrap()));
            }
        }
    }
    Ok(())
}