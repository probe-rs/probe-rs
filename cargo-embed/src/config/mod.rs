use crate::rttui::channel::ChannelConfig;
use probe_rs::WireProtocol;
use serde::Deserialize;

lazy_static::lazy_static! {
    /// This is an example for using doc comment attributes
    pub static ref CONFIG: Config = Config::new().expect("Config could not be loaded.");
}

/// The main struct holding all the possible config options.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub general: General,
    pub flashing: Flashing,
    pub probe: Probe,
    pub rtt: Rtt,
    pub gdb: Gdb,
}

/// The probe config struct holding all the possible probe options.
#[derive(Debug, Deserialize)]
pub struct Probe {
    pub probe_selector: Option<String>,
    pub protocol: WireProtocol,
    pub speed: Option<u32>,
}

/// The flashing config struct holding all the possible flashing options.
#[derive(Debug, Deserialize)]
pub struct Flashing {
    pub enabled: bool,
    pub halt_afterwards: bool,
    pub restore_unwritten_bytes: bool,
    pub flash_layout_output_path: Option<String>,
}

/// The general config struct holding all the possible general options.
#[derive(Debug, Deserialize)]
pub struct General {
    pub chip: Option<String>,
    pub chip_descriptions: Vec<String>,
    pub log_level: log::Level,
}

/// The rtt config struct holding all the possible rtt options.
#[derive(Debug, Deserialize)]
pub struct Rtt {
    pub enabled: bool,
    pub channels: Vec<ChannelConfig>,
    /// Connection timeout in ms.
    pub timeout: usize,
    /// Whether to show timestamps in RTTUI
    pub show_timestamps: bool,
}

/// The gdb config struct holding all the possible gdb options.
#[derive(Debug, Deserialize)]
pub struct Gdb {
    pub enabled: bool,
    pub gdb_connection_string: Option<String>,
}

impl Config {
    pub fn new() -> Result<Self, config::ConfigError> {
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

        // You can deserialize (and thus freeze) the entire configuration
        s.try_into()
    }
}
