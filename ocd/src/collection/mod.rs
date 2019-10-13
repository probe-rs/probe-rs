pub mod cores;
pub mod targets;

use std::io;
use std::fs::{self, DirEntry};
use std::path::{
    Path,
    PathBuf,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

use crate::target::Target;
use crate::target::Core;

pub fn get_target(name: impl AsRef<str>) -> Option<Target> {
    let mut map: HashMap<String, Target> = HashMap::new();

    load_targets(dirs::home_dir().map(|home| home.join(".config/probe-rs/targets")), &mut map);

    let name: String = name.as_ref().into();

    map.get(&name.to_ascii_lowercase()).map(|target| target.clone())
}

pub fn load_targets(root: Option<PathBuf>, map: &mut HashMap<String, Target>) {
    if let Some(root) = root {
        visit_dirs(root.as_path(), map).unwrap();
    } else {
        log::warn!("Home directory could not be determined while loading targets.");
    }
}

pub fn load_targets_from_dir(dir: &DirEntry, map: &mut HashMap<String, Target>) {
    match File::open(dir.path()) {
        Ok(file) => {
            let reader = BufReader::new(file);

            // Read the JSON contents of the file as an instance of `User`.
            match serde_yaml::from_reader(reader) as serde_yaml::Result<Target> {
                Ok(mut target) => {
                    target.name.make_ascii_lowercase();
                    map.insert(target.name.clone(), target);
                },
                Err(e) => { log::warn!("Error loading chip definition: {}", e) }
            }
        },
        Err(e) => {
            log::info!("Unable to load file {:?}.", dir.path());
            log::info!("Reason: {:?}", e);
        }
    }
}

// one possible implementation of walking a directory only visiting files
fn visit_dirs(dir: &Path, map: &mut HashMap<String, Target>) -> io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, map)?;
            } else {
                load_targets_from_dir(&entry, map);
            }
        }
    }
    Ok(())
}

pub fn get_core(name: impl AsRef<str>) -> Option<Box<dyn Core>> {
    let map: HashMap<String, Box<dyn Core>> = hashmap!{
        "M0".to_string() => Box::new(self::cores::m0::M0) as _,
    };

    map.get(name.as_ref()).map(|target| target.clone())
}