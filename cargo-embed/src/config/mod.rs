use std::collections::HashMap;

use crate::rttui::channel::ChannelConfig;
use derivative::Derivative;
use probe_rs::WireProtocol;
use serde::{Deserialize, Serialize};

/// A struct which holds all configs.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Configs(HashMap<String, Config>);

/// The main struct holding all the possible config options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub flashing: Flashing,
    #[serde(default)]
    pub probe: Probe,
    #[serde(default)]
    pub rtt: Rtt,
    #[serde(default)]
    pub gdb: Gdb,
}

/// The probe config struct holding all the possible probe options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Probe {
    pub probe_selector: Option<String>,
    #[serde(default = "default_protocol")]
    #[derivative(Default(value = "default_protocol()"))]
    pub protocol: WireProtocol,
    #[serde(default)]
    pub speed: Option<u32>,
}

fn default_protocol() -> WireProtocol {
    WireProtocol::Swd
}

/// The flashing config struct holding all the possible flashing options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Flashing {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub halt_afterwards: bool,
    #[serde(default)]
    pub restore_unwritten_bytes: bool,
    #[serde(default)]
    pub flash_layout_output_path: Option<String>,
}

/// The general config struct holding all the possible general options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct General {
    #[serde(default)]
    pub chip: Option<String>,
    #[serde(default)]
    pub chip_descriptions: Vec<String>,
    #[serde(default = "default_log_level")]
    #[derivative(Default(value = "default_log_level()"))]
    pub log_level: log::Level,
    #[serde(default)]
    pub derives: Option<String>,
}

fn default_log_level() -> log::Level {
    log::Level::Warn
}

/// The rtt config struct holding all the possible rtt options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Rtt {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
    /// Connection timeout in ms.
    #[serde(default = "default_timeout")]
    #[derivative(Default(value = "default_timeout()"))]
    pub timeout: usize,
    /// Whether to show timestamps in RTTUI
    pub show_timestamps: bool,
}

fn default_timeout() -> usize {
    3000
}

/// The gdb config struct holding all the possible gdb options.
#[derive(Debug, Deserialize, Serialize, Derivative)]
#[derivative(Default)]
pub struct Gdb {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
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

        let map: HashMap<String, Config> = s.try_into()?;

        let config = &map[name.as_ref()];

        let mut s = config::Config::new();

        Self::apply(&mut s, config, &map)?;

        // You can deserialize (and thus freeze) the entire configuration
        s.try_into()
    }

    pub fn apply(
        s: &mut config::Config,
        config: &Config,
        map: &HashMap<String, Config>,
    ) -> Result<(), config::ConfigError> {
        println!("{:?}", config);
        // If this config derives from another config, merge the other config first.
        // Do this recursively.
        if let Some(derives) = &config.general.derives {
            if let Some(dconfig) = map.get(derives) {
                Self::apply(s, dconfig, map)?;
            }
        }

        println!("{:#?}", s);

        // Merge this current config.
        s.merge(config::File::from_str(
            // This unwrap can never fail as we just deserialized this. The reverse has to work!
            &serde_json::to_string(&config).unwrap(),
            config::FileFormat::Json,
        ))
        .map(|_| ())
    }
}
