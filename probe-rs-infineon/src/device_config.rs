//! AURIX device descriptions of the Infineon AURIX Flasher (`DeviceConfigs/*.json`).
//!
//! The AURIX Flasher describes every device it supports in a JSON file: the chip and JTAG IDs
//! that identify it, the flash loader it uses (`flaxFileName`), and its memories with their
//! physical and logical sectors. probe-rs matches the connected device to one of these files by
//! its JTAG ID and SCU_CHIPID and takes the PFLASH sector map from it.
//!
//! The files are Infineon licensed material and are not part of probe-rs. They are read from the
//! user's installation:
//!
//! 1. the directories in `PROBE_RS_AURIX_DEVICE_CONFIGS` (a path list),
//! 2. `bin/aurixflasher/DeviceConfigs` of the AURIX TCF debugger extension for VS Code
//!    (`~/.vscode/extensions/*aurix-tcf*`), newest version first.
//!
//! A directory with a `devices.json` lists its device files there, in order; otherwise every
//! `*.json` file of the directory is read.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The environment variable with the directories of the AURIX Flasher device files.
pub const CONFIG_DIR_ENV: &str = "PROBE_RS_AURIX_DEVICE_CONFIGS";

/// The flash loader family of the TC2xx devices.
pub const FLAX_TC2XX: &str = "flax_TC2xx.hex";

/// One sector of a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigSector {
    /// The first (non-cached) address.
    pub base: u32,
    /// The size in bytes.
    pub size: u32,
    /// The AURIX Flasher does not write this sector.
    pub restricted: bool,
}

/// A memory of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    /// The name, for example `ProgramFlash`, `DataFlash` or `CPU0 DSPR`.
    pub name: String,
    /// The first (non-cached) address.
    pub start: u32,
    /// The last address.
    pub end: u32,
    /// The logical sectors.
    pub sectors: Vec<ConfigSector>,
}

/// One device file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceConfig {
    /// The file it was read from.
    pub file: PathBuf,
    /// The device name, for example `TC27x D step`.
    pub device_name: String,
    /// The flash loader of the AURIX Flasher, which also names the flash controller family.
    pub flax_file_name: String,
    /// The JTAG IDs of the device.
    pub jtag_ids: Vec<u32>,
    /// The SCU_CHIPID values of the device.
    pub chip_ids: Vec<u32>,
    /// The memories.
    pub memories: Vec<Memory>,
}

/// How well a device file matches a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchQuality {
    /// The JTAG ID matches, the chip ID does not.
    JtagId,
    /// The JTAG ID and the chip ID match.
    Exact,
}

/// Errors reading a device file.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum ConfigError {
    /// Cannot read {0}: {1}
    Io(PathBuf, std::io::Error),
    /// {0} is not an AURIX Flasher device file: {1}
    Json(PathBuf, serde_json::Error),
    /// {0}: '{1}' is not a hexadecimal number.
    Number(PathBuf, String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawConfig {
    device_name: String,
    #[serde(default)]
    flax_file_name: String,
    #[serde(default)]
    registers: Vec<RawRegister>,
    #[serde(default)]
    memory: Vec<RawMemory>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRegister {
    register_name: String,
    #[serde(default)]
    register_value: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMemory {
    memory_name: String,
    memory_start_address: String,
    memory_end_address: String,
    #[serde(default)]
    logical_sectors: Vec<RawSector>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSector {
    sector_start_address: String,
    sector_size: String,
    #[serde(default)]
    restricted: bool,
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(default)]
    devices: Vec<String>,
}

fn hex(file: &Path, text: &str) -> Result<u32, ConfigError> {
    let digits = text.trim();
    let digits = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .unwrap_or(digits);
    u32::from_str_radix(digits, 16).map_err(|_| ConfigError::Number(file.into(), text.into()))
}

impl DeviceConfig {
    /// Parses the device file `file` with the content `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            serde_json::from_str(text).map_err(|e| ConfigError::Json(file.into(), e))?;
        let ids = |name: &str| -> Result<Vec<u32>, ConfigError> {
            raw.registers
                .iter()
                .filter(|r| r.register_name.eq_ignore_ascii_case(name))
                .flat_map(|r| &r.register_value)
                .map(|value| hex(file, value))
                .collect()
        };
        let memories = raw
            .memory
            .iter()
            .map(|m| {
                Ok(Memory {
                    name: m.memory_name.clone(),
                    start: hex(file, &m.memory_start_address)?,
                    end: hex(file, &m.memory_end_address)?,
                    sectors: m
                        .logical_sectors
                        .iter()
                        .map(|s| {
                            Ok(ConfigSector {
                                base: hex(file, &s.sector_start_address)?,
                                size: hex(file, &s.sector_size)?,
                                restricted: s.restricted,
                            })
                        })
                        .collect::<Result<_, ConfigError>>()?,
                })
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self {
            file: file.into(),
            device_name: raw.device_name,
            flax_file_name: raw.flax_file_name,
            jtag_ids: ids("JTAGID")?,
            chip_ids: ids("ChipID")?,
            memories,
        })
    }

    /// The PFLASH (`ProgramFlash`).
    pub fn program_flash(&self) -> Option<&Memory> {
        self.memories
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("ProgramFlash"))
    }

    /// Whether the device file describes a device with these IDs.
    pub fn matches(&self, jtag_id: u32, chip_id: Option<u32>) -> Option<MatchQuality> {
        if !self.jtag_ids.contains(&jtag_id) {
            return None;
        }
        match chip_id {
            Some(chip_id) if self.chip_ids.contains(&chip_id) => Some(MatchQuality::Exact),
            _ => Some(MatchQuality::JtagId),
        }
    }
}

/// The directories that are searched for device files, in order.
pub fn config_dirs() -> Vec<PathBuf> {
    let mut dirs = std::env::var_os(CONFIG_DIR_ENV)
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    if let Some(home) = home
        && let Ok(entries) = std::fs::read_dir(Path::new(&home).join(".vscode").join("extensions"))
    {
        let mut extensions = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("aurix-tcf"))
            })
            .collect::<Vec<_>>();
        // The newest version first.
        extensions.sort();
        extensions.reverse();
        dirs.extend(
            extensions
                .into_iter()
                .map(|p| p.join("bin").join("aurixflasher").join("DeviceConfigs")),
        );
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// Reads the device files of `dir`. Files that cannot be read are skipped with a warning.
pub fn load_dir(dir: &Path) -> Vec<DeviceConfig> {
    let index = dir.join("devices.json");
    let files = match std::fs::read_to_string(&index)
        .ok()
        .and_then(|text| serde_json::from_str::<RawIndex>(&text).ok())
    {
        Some(index) => index.devices.into_iter().map(|f| dir.join(f)).collect(),
        None => std::fs::read_dir(dir)
            .map(|entries| {
                let mut files = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().is_some_and(|e| e == "json")
                            && p.file_name().is_some_and(|n| n != "devices.json")
                    })
                    .collect::<Vec<_>>();
                files.sort();
                files
            })
            .unwrap_or_default(),
    };
    files
        .into_iter()
        .filter_map(|file| {
            let parsed = std::fs::read_to_string(&file)
                .map_err(|e| ConfigError::Io(file.clone(), e))
                .and_then(|text| DeviceConfig::parse(&file, &text));
            match parsed {
                Ok(config) => Some(config),
                Err(error) => {
                    tracing::warn!("{error}");
                    None
                }
            }
        })
        .collect()
}

/// Finds the device file for a device in `dirs`: an exact match (JTAG ID and chip ID) wins,
/// then the first file with the JTAG ID.
pub fn find_in(dirs: &[PathBuf], jtag_id: u32, chip_id: Option<u32>) -> Option<DeviceConfig> {
    let mut best: Option<(MatchQuality, DeviceConfig)> = None;
    for dir in dirs {
        for config in load_dir(dir) {
            if let Some(quality) = config.matches(jtag_id, chip_id)
                && best.as_ref().is_none_or(|(q, _)| quality > *q)
            {
                best = Some((quality, config));
            }
        }
        if matches!(best, Some((MatchQuality::Exact, _))) {
            break;
        }
    }
    if let Some((MatchQuality::JtagId, config)) = &best {
        tracing::warn!(
            "No AURIX device file lists chip ID {}; using {} for JTAG ID {jtag_id:#010x}",
            chip_id.map_or("?".to_string(), |c| format!("{c:#010x}")),
            config.device_name
        );
    }
    best.map(|(_, config)| config)
}

/// Finds the device file for a device in [`config_dirs`].
pub fn find(jtag_id: u32, chip_id: Option<u32>) -> Option<DeviceConfig> {
    find_in(&config_dirs(), jtag_id, chip_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device file in the AURIX Flasher format with made-up values.
    const SAMPLE: &str = r#"{
        "deviceName": "Sample TC27x",
        "flaxFileName": "flax_TC2xx.hex",
        "busWidth": 32,
        "registers": [
            { "registerName": "ChipID", "registerValue": ["0x47427060", "0x47427061"] },
            { "registerName": "JTAGID", "registerValue": ["0x401DA083"] }
        ],
        "memory": [
            {
                "memoryName": "ProgramFlash",
                "memoryStartCachedAddress": "0x80000000",
                "memoryStartAddress": "0xA0000000",
                "memoryEndAddress": "0xA0007FFF",
                "burstSize": "0x100",
                "physicalSectors": [],
                "logicalSectors": [
                    { "sectorStartAddress": "0xA0000000", "sectorEndAddress": "0xA0003FFF", "sectorSize": "0x4000", "restricted": true },
                    { "sectorStartAddress": "0xA0004000", "sectorEndAddress": "0xA0007FFF", "sectorSize": "0x4000", "restricted": false }
                ]
            },
            { "memoryName": "CPU0 DSPR", "memoryStartAddress": "0x70000000", "memoryEndAddress": "0x7001BFFF", "burstSize": "" }
        ]
    }"#;

    fn sample_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "probe-rs-device-configs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }

    #[test]
    fn a_device_file_is_parsed() {
        let config = DeviceConfig::parse(Path::new("sample.json"), SAMPLE).unwrap();
        assert_eq!(config.device_name, "Sample TC27x");
        assert_eq!(config.flax_file_name, FLAX_TC2XX);
        assert_eq!(config.jtag_ids, [0x401D_A083]);
        assert_eq!(config.chip_ids, [0x4742_7060, 0x4742_7061]);
        let pflash = config.program_flash().unwrap();
        assert_eq!((pflash.start, pflash.end), (0xA000_0000, 0xA000_7FFF));
        assert_eq!(
            pflash.sectors,
            [
                ConfigSector {
                    base: 0xA000_0000,
                    size: 0x4000,
                    restricted: true
                },
                ConfigSector {
                    base: 0xA000_4000,
                    size: 0x4000,
                    restricted: false
                }
            ]
        );
        assert_eq!(config.memories.len(), 2);
    }

    #[test]
    fn the_chip_id_decides_between_files_of_one_jtag_id() {
        let other_step = SAMPLE
            .replace("Sample TC27x", "Other step")
            .replace("\"0x47427060\", \"0x47427061\"", "\"0x47467172\"");
        let dir = sample_dir(
            "match",
            &[
                ("devices.json", r#"{ "devices": ["b.json", "a.json"] }"#),
                ("a.json", SAMPLE),
                ("b.json", &other_step),
                ("unlisted.json", "not json"),
            ],
        );
        let dirs = [dir.clone()];
        let exact = find_in(&dirs, 0x401D_A083, Some(0x4742_7061)).unwrap();
        assert_eq!(exact.device_name, "Sample TC27x");
        let step = find_in(&dirs, 0x401D_A083, Some(0x4746_7172)).unwrap();
        assert_eq!(step.device_name, "Other step");
        // An unknown chip ID falls back to the first file with the JTAG ID (devices.json order).
        let fallback = find_in(&dirs, 0x401D_A083, Some(0x1234_5678)).unwrap();
        assert_eq!(fallback.device_name, "Other step");
        assert!(find_in(&dirs, 0x1020_7083, None).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn without_an_index_every_json_file_is_read() {
        let dir = sample_dir("no-index", &[("a.json", SAMPLE), ("broken.json", "{")]);
        let configs = load_dir(&dir);
        assert_eq!(configs.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_numbers_are_reported() {
        let text = SAMPLE.replace(
            "0x4000\", \"restricted\": true",
            "0xZZ\", \"restricted\": true",
        );
        assert!(matches!(
            DeviceConfig::parse(Path::new("x.json"), &text),
            Err(ConfigError::Number(_, _))
        ));
    }
}
