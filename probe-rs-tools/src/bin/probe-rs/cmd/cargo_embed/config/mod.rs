use anyhow::bail;
use figment::{
    providers::{Format, Json, Toml, Yaml},
    Figment,
};
use probe_rs::probe::WireProtocol;
use probe_rs::rtt::ChannelMode;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

use crate::util::{logging::LevelFilter, rtt::DataFormat};

use super::rttui::tab::TabConfig;

/// A struct which holds all configs.
#[derive(Debug, Clone)]
pub struct Configs {
    figment: Figment,
}

/// The main struct holding all the possible config options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub flashing: Flashing,
    pub reset: Reset,
    pub probe: Probe,
    pub rtt: Rtt,
    pub gdb: Gdb,
}

/// The probe config struct holding all the possible probe options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub usb_vid: Option<String>,
    pub usb_pid: Option<String>,
    pub serial: Option<String>,
    pub protocol: WireProtocol,
    pub speed: Option<u32>,
}

/// The flashing config struct holding all the possible flashing options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Flashing {
    pub enabled: bool,
    pub restore_unwritten_bytes: bool,
    pub flash_layout_output_path: Option<String>,
    pub do_chip_erase: bool,
    pub disable_double_buffering: bool,
    pub preverify: bool,
    pub verify: bool,
}

/// The reset config struct holding all the possible reset options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reset {
    pub enabled: bool,
    pub halt_afterwards: bool,
}

/// The general config struct holding all the possible general options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub chip: Option<String>,
    pub chip_descriptions: Vec<String>,
    pub log_level: Option<LevelFilter>,
    pub derives: Option<String>,
    /// Use this flag to assert the nreset & ntrst pins during attaching the probe to the chip.
    pub connect_under_reset: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
// Note: default values are defined in `RttChannelConfig`.
pub struct UpChannelConfig {
    pub channel: usize,
    #[serde(default)]
    pub mode: Option<ChannelMode>,
    #[serde(default)]
    pub format: Option<DataFormat>,
    #[serde(default)]
    pub show_location: Option<bool>,
    #[serde(default)]
    pub socket: Option<SocketAddr>,
    // TODO: it should be possible to move these into DataFormat
    #[serde(default)]
    /// Controls the inclusion of timestamps for [`DataFormat::String`] and [`DataFormat::Defmt`].
    pub show_timestamps: Option<bool>,
    #[serde(default)]
    /// Controls the output format for DataFormat::Defmt.
    pub log_format: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownChannelConfig {
    pub channel: usize,
    #[serde(default)]
    pub mode: Option<ChannelMode>,
}

/// The rtt config struct holding all the possible rtt options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rtt {
    pub enabled: bool,
    /// Up mode, when not specified per-channel.  Target picks if neither is set
    pub up_mode: Option<ChannelMode>,
    /// Channels to be displayed, and options for them
    pub up_channels: Vec<UpChannelConfig>,
    /// Channels to be displayed, and options for them
    pub down_channels: Vec<DownChannelConfig>,
    /// UI tab configuration
    pub tabs: Vec<TabConfig>,
    /// Connection timeout in ms.
    #[serde(with = "duration_ms")]
    pub timeout: Duration,
    /// Whether to save rtt history buffer on exit to file named history.txt
    pub log_enabled: bool,
    /// Where to save rtt history buffer relative to manifest path.
    pub log_path: PathBuf,
}

impl Rtt {
    /// Returns the configuration for the specified up channel number, if it exists.
    pub fn up_channel_config(&self, channel_number: usize) -> Option<&UpChannelConfig> {
        self.up_channels
            .iter()
            .find(|ch| ch.channel == channel_number)
    }
}

mod duration_ms {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u128(duration.as_millis())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        u64::deserialize(deserializer).map(Duration::from_millis)
    }
}

/// The gdb config struct holding all the possible gdb options.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gdb {
    pub enabled: bool,
    pub gdb_connection_string: Option<String>,
}

impl Configs {
    pub fn new(conf_dir: PathBuf) -> Configs {
        // Start off by merging in the default configuration file.
        let mut figments =
            Figment::new().merge(Toml::string(include_str!("default.toml")).nested());

        // Ordered list of config files, which are handled in the order specified here.
        let config_files = [
            // The following files are intended to be project-specific and would normally be
            // included in a project's source repository.
            ".embed",
            "Embed",
            // The following files are intended to hold personal or exceptional settings, which
            // are not useful for other users, and would NOT normally be included in a project's
            // source repository.
            ".embed.local",
            "Embed.local",
        ];

        for file in &config_files {
            let mut toml_path: std::path::PathBuf = conf_dir.clone();
            toml_path.push(format!("{file}.toml"));

            let mut json_path = conf_dir.clone();
            json_path.push(format!("{file}.json"));

            let mut yaml_path = conf_dir.clone();
            yaml_path.push(format!("{file}.yaml"));

            let mut yml_path = conf_dir.clone();
            yml_path.push(format!("{file}.yml"));

            figments = Figment::from(figments)
                .merge(Toml::file(toml_path).nested())
                .merge(Json::file(json_path).nested())
                .merge(Yaml::file(yaml_path).nested())
                .merge(Yaml::file(yml_path).nested());
        }
        Configs { figment: figments }
    }

    pub fn merge(&mut self, conf_file: PathBuf) -> anyhow::Result<()> {
        let original = self.figment.clone();
        self.figment = match conf_file.extension().and_then(|e| e.to_str()) {
            Some("toml") => original.merge(Toml::file(conf_file).nested()),
            Some("json") => original.merge(Json::file(conf_file).nested()),
            Some("yml" | "yaml") => original.merge(Yaml::file(conf_file).nested()),
            _ => {
                return Err(anyhow::anyhow!(
                "File format not recognized from extension (supported: .toml, .json, .yaml / .yml)"
            ))
            }
        };
        Ok(())
    }

    pub fn prof_names(&self) -> Vec<String> {
        self.figment
            .profiles()
            .map(|p| String::from(p.as_str().as_str()))
            .collect()
    }

    /// Extract the requested config, but only if the profile has been explicitly defined in the
    /// configuration files etc. (selecting an arbitrary undefined profile with Figment will coerce
    /// it into existence - inheriting from the default config).
    pub fn select_defined(self: Configs, name: &str) -> anyhow::Result<Config> {
        let defined_profiles = self.prof_names();
        let requested_profile_defined: bool = defined_profiles
            .iter()
            .any(|p| p.to_lowercase() == name.to_lowercase());

        let figext: figment::error::Result<Config> = self.figment.select(name).extract();
        match figext {
            Err(figerr) => {
                // Join all the figment errors into a multiline string.
                bail!(
                    "Failed to parse supplied configuration:\n{}",
                    figerr
                        .into_iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<String>>()
                        .join("\n")
                );
            }
            Ok(config) => {
                // Gross errors (e.g. config file syntax problems) have already been caught by the
                // other match arm.  Guard against Figment coercing previously undefined profiles
                // into existance.
                if !requested_profile_defined {
                    bail!(
                        "the requested configuration profile \"{}\" hasn't been defined (defined profiles: {})",
                        name,
                        defined_profiles.join(", ")
                    );
                }
                Ok(config)
            }
        }
    }
    #[cfg(test)]
    pub fn new_with_test_data(conf_dir: PathBuf) -> Configs {
        let mut cfs = Configs::new(conf_dir);
        cfs.figment = cfs.figment.merge(
            Toml::string(
                r#"
            [default]
               bogusInvalidItem = "oops"
               "#,
            )
            .nested(),
        );
        cfs
    }
}

#[cfg(test)]
mod test {
    use super::Configs;

    #[test]
    fn default_profile() {
        // Ensure the default config can be parsed.
        let configs = Configs::new(std::env::current_dir().unwrap());
        let _config = configs.select_defined("default").unwrap();
    }
    #[test]
    fn non_existant_profile_is_error() {
        // Selecting a non-existant profile.
        let configs = Configs::new(std::env::current_dir().unwrap());
        let _noprofile = configs.select_defined("nonexxistantprofle").unwrap_err();
    }
    #[test]
    fn unknown_config_items_in_config_fail() {
        // Selecting a profile with an invalid item.
        let configs = Configs::new_with_test_data(std::env::current_dir().unwrap());
        let _superfluous: anyhow::Error = configs.select_defined("default").unwrap_err();
    }
    #[test]
    fn file_name_patterns() {
        // Existence of files is not tested here, so it is fine to use a file that does not exist
        Configs::new(std::env::current_dir().unwrap())
            .merge("nonexistent-file.yml".into())
            .unwrap();
        Configs::new(std::env::current_dir().unwrap())
            .merge("nonexistent-file.unknown".into())
            .unwrap_err();
    }
}
