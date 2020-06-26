use std::collections::HashMap;

use crate::rttui::channel::ChannelConfig;
use probe_rs::WireProtocol;
use serde::{Deserialize, Serialize};

/// A struct which holds all configs.
#[derive(Debug, Deserialize, Serialize)]
pub struct Configs(HashMap<String, Config>);

/// The main struct holding all the possible config options.
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub general: General,
    pub flashing: Flashing,
    pub probe: Probe,
    pub rtt: Rtt,
    pub gdb: Gdb,
}

/// The probe config struct holding all the possible probe options.
#[derive(Debug, Deserialize, Serialize)]
pub struct Probe {
    pub usb_vid: Option<String>,
    pub usb_pid: Option<String>,
    pub serial: Option<String>,
    pub protocol: WireProtocol,
    pub speed: Option<u32>,
}

/// The flashing config struct holding all the possible flashing options.
#[derive(Debug, Deserialize, Serialize)]
pub struct Flashing {
    pub enabled: bool,
    pub halt_afterwards: bool,
    pub restore_unwritten_bytes: bool,
    pub flash_layout_output_path: Option<String>,
}

/// The general config struct holding all the possible general options.
#[derive(Debug, Deserialize, Serialize)]
pub struct General {
    pub chip: Option<String>,
    pub chip_descriptions: Vec<String>,
    pub log_level: log::Level,
    pub derives: Option<String>,
}

/// The rtt config struct holding all the possible rtt options.
#[derive(Debug, Deserialize, Serialize)]
pub struct Rtt {
    pub enabled: bool,
    pub channels: Vec<ChannelConfig>,
    /// Connection timeout in ms.
    pub timeout: usize,
    /// Whether to show timestamps in RTTUI
    pub show_timestamps: bool,
}

/// The gdb config struct holding all the possible gdb options.
#[derive(Debug, Deserialize, Serialize)]
pub struct Gdb {
    pub enabled: bool,
    pub gdb_connection_string: Option<String>,
}

impl Configs {
    pub fn new(name: impl AsRef<str>) -> Result<Config, config::ConfigError> {
        let mut s = config::Config::new();

        // Start off by merging in the default configuration file.
        s.merge(config::File::from_str(
            include_str!("default.toml"),
            config::FileFormat::Toml,
        ))?;

        // Merge in the project-specific configuration files.
        // These files may be added to your git repo.
        s.merge(config::File::with_name(".embed").required(false))?;
        s.merge(config::File::with_name("Embed").required(false))?;

        // Merge in the local configuration files.
        // These files should not be added to your git repo.
        s.merge(config::File::with_name(".embed.local").required(false))?;
        s.merge(config::File::with_name("Embed.local").required(false))?;

        let map: HashMap<String, serde_json::value::Value> = s.try_into()?;

        let config = &map[name.as_ref()];

        let mut s = config::Config::new();

        Self::apply(name.as_ref(), &mut s, config, &map)?;

        // You can deserialize (and thus freeze) the entire configuration
        s.try_into()
    }

    pub fn apply(
        name: &str,
        s: &mut config::Config,
        config: &serde_json::value::Value,
        map: &HashMap<String, serde_json::value::Value>,
    ) -> Result<(), config::ConfigError> {
        // If this config derives from another config, merge the other config first.
        // Do this recursively.
        if let Some(derives) = config
            .get("general")
            .and_then(|g| g.get("derives").and_then(|d| d.as_str()))
            .or(Some("default"))
        {
            if derives == name {
                log::warn!("Endless recursion within the {} config.", derives);
            } else if let Some(dconfig) = map.get(derives) {
                Self::apply(derives, s, dconfig, map)?;
            }
        }

        // Merge this current config.
        s.merge(config::File::from_str(
            // This unwrap can never fail as we just deserialized this. The reverse has to work!
            &serde_json::to_string(&config).unwrap(),
            config::FileFormat::Json,
        ))
        .map(|_| ())
    }
}
