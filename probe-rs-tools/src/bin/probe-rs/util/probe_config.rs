//! The probe table of the configuration file: settings for each probe.

use std::collections::HashMap;

use probe_rs::probe::{
    DebugProbeError, DebugProbeSelector, Probe, WireProtocol,
    ch347::{Ch347, Ch347Config},
    list::{ProbeListItem, ProbeLister},
};
use serde::{Deserialize, Deserializer, Serialize};

/// Settings for each probe, keyed by probe selector (`VID:PID`, `VID:PID:serial` or
/// `VID:PID-interface`).
///
/// ```toml
/// [probes."0403:6010-1"]
/// speed = 4000
/// protocol = "swd"
///
/// [probes."1a86:55dd"]
/// protocol = "swd"
/// reset_line_connected = true
///
/// [probes."1a86:55dd".driver]
/// type = "ch347"
/// reset_pin = "trst"
/// ```
#[derive(Debug, Default, Clone, Serialize)]
#[serde(transparent)]
pub struct ProbeConfigs(HashMap<DebugProbeSelector, ProbeConfig>);

impl<'de> Deserialize<'de> for ProbeConfigs {
    /// Parses the selectors. Two keys must not select the same probes.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut keys = HashMap::new();
        let mut configs = HashMap::new();
        for (key, config) in HashMap::<String, ProbeConfig>::deserialize(deserializer)? {
            let selector = key.parse::<DebugProbeSelector>().map_err(|error| {
                serde::de::Error::custom(format!(
                    "Invalid probe selector `{key}` in `probes`: {error}"
                ))
            })?;
            if let Some(other) = keys.insert(selector.clone(), key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "`probes` has two entries for the same probes: `{other}` and `{key}`. Use the same spelling in all configuration files."
                )));
            }
            configs.insert(selector, config);
        }
        Ok(Self(configs))
    }
}

/// Settings for one probe, or for all probes that a selector matches. A `None` field keeps the
/// default.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeConfig {
    /// The protocol speed in kHz, if the command does not set it.
    pub speed: Option<u32>,
    /// The wire protocol, if the command does not set it.
    #[serde(deserialize_with = "deserialize_protocol")]
    pub protocol: Option<WireProtocol>,
    /// The reset line of the probe is connected to the target's nRESET. `probe-rs reset` then uses a
    /// pin reset. The other commands use a pin reset only on request, for example for connect under
    /// reset. If `false`, probe-rs does not assert the reset line, and operations that need a pin
    /// reset fail.
    /// A WCH-LinkE or WCH-LinkW resets with its RST pin only if this is `true`.
    pub reset_line_connected: Option<bool>,
    /// Settings of the probe driver.
    pub driver: Option<DriverConfig>,
}

/// Settings of one probe driver. `type` selects the driver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DriverConfig {
    Ch347(Ch347Config),
}

impl DriverConfig {
    /// Applies the settings to `probe`. Call this before attach.
    pub fn apply(&self, probe: &mut Probe) -> Result<(), DebugProbeError> {
        let probe_name = probe.get_name();
        let mismatch = || {
            DebugProbeError::Other(format!(
                "The driver settings are for a {}, but the probe is a {probe_name}",
                self.driver_name(),
            ))
        };
        match self {
            DriverConfig::Ch347(config) => probe
                .try_into::<Ch347>()
                .ok_or_else(mismatch)?
                .configure(config),
        }
    }

    fn driver_name(&self) -> &'static str {
        match self {
            DriverConfig::Ch347(_) => "CH347",
        }
    }
}

fn deserialize_protocol<'de, D>(deserializer: D) -> Result<Option<WireProtocol>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|protocol| protocol.parse().map_err(serde::de::Error::custom))
        .transpose()
}

impl ProbeConfig {
    /// Sets the fields of `self` that `other` sets.
    fn merge(&mut self, other: &ProbeConfig) {
        self.speed = other.speed.or(self.speed);
        self.protocol = other.protocol.or(self.protocol);
        self.reset_line_connected = other.reset_line_connected.or(self.reset_line_connected);
        // Driver settings are one setting.
        if other.driver.is_some() {
            self.driver = other.driver.clone();
        }
    }
}

impl ProbeConfigs {
    /// Returns the settings for `probe`. An entry with a serial number overrides an entry with an
    /// interface, which overrides an entry with neither: `VID:PID:serial` overrides `VID:PID-1`,
    /// which overrides `VID:PID`.
    pub fn resolve(&self, probe: &DebugProbeSelector) -> ProbeConfig {
        let mut matching = self
            .0
            .iter()
            .filter(|(selector, _)| selector.matches_selector(probe))
            .collect::<Vec<_>>();
        matching.sort_by_key(|(selector, _)| {
            (
                selector.serial_number.is_some(),
                selector.interface.is_some(),
            )
        });

        let mut resolved = ProbeConfig::default();
        for (_, config) in matching {
            resolved.merge(config);
        }
        resolved
    }

    /// Returns whether an entry with a serial number can match `probe`. Only these entries need
    /// the serial number of the probe.
    fn needs_serial_number(&self, probe: &DebugProbeSelector) -> bool {
        probe.serial_number.is_none()
            && self.0.keys().any(|selector| {
                selector.serial_number.is_some()
                    && (selector.vendor_id, selector.product_id)
                        == (probe.vendor_id, probe.product_id)
            })
    }
}

/// Opens probes with the settings of the probe table. Set the command line options after this.
#[derive(Debug)]
pub struct ConfiguredLister<L> {
    inner: L,
    configs: ProbeConfigs,
}

impl<L: ProbeLister> ConfiguredLister<L> {
    pub fn new(inner: L, configs: ProbeConfigs) -> Self {
        Self { inner, configs }
    }

    /// Adds the serial number of the probe, so that the entries with a serial number match.
    fn complete_selector(&self, selector: &DebugProbeSelector) -> DebugProbeSelector {
        let mut selector = selector.clone();
        if self.configs.needs_serial_number(&selector)
            && let [info] = self.inner.list(Some(&selector)).as_slice()
        {
            selector.serial_number = info.serial_number.clone();
        }
        selector
    }
}

impl<L: ProbeLister> ProbeLister for ConfiguredLister<L> {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
        let mut probe = self.inner.open(selector)?;
        if self.configs.0.is_empty() {
            return Ok(probe);
        }

        let config = self.configs.resolve(&self.complete_selector(selector));
        if let Some(driver) = &config.driver {
            driver.apply(&mut probe)?;
        }
        if let Some(connected) = config.reset_line_connected {
            probe.set_reset_line_connected(connected);
        }
        if let Some(protocol) = config.protocol {
            probe.select_protocol(protocol)?;
        }
        if let Some(speed) = config.speed {
            probe.set_speed(speed)?;
            let actual_speed = probe.speed_khz();
            if actual_speed < speed {
                tracing::warn!(
                    "Unable to use the speed of {speed} kHz from the probe table, actual speed used is {actual_speed} kHz"
                );
            }
        }
        Ok(probe)
    }

    fn list_with_access(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        self.inner.list_with_access(selector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use figment2::Figment;
    use figment2::providers::{Format as _, Toml};
    use probe_rs::integration::FakeProbe;
    use probe_rs::probe::{DebugProbeInfo, cmsisdap::CmsisDapFactory};

    fn configs(toml: &str) -> ProbeConfigs {
        Figment::from(Toml::string(toml)).extract().unwrap()
    }

    fn is_rejected(toml: &str) -> bool {
        Figment::from(Toml::string(toml))
            .extract::<ProbeConfigs>()
            .is_err()
    }

    fn probe(selector: &str) -> DebugProbeSelector {
        selector.parse().unwrap()
    }

    #[test]
    fn specific_entry_overrides_general_entry() {
        let configs = configs(
            r#"
            ["0403:6010"]
            speed = 1000
            protocol = "jtag"

            ["0403:6010:ABC"]
            speed = 4000

            ["0403:6010-1"]
            speed = 2000
            protocol = "swd"

            ["1234:5678"]
            speed = 1
            "#,
        );

        let config = configs.resolve(&probe("0403:6010:ABC"));
        assert_eq!(config.speed, Some(4000));
        assert_eq!(config.protocol, Some(WireProtocol::Jtag));

        let config = configs.resolve(&probe("0403:6010-1:OTHER"));
        assert_eq!(config.speed, Some(2000));
        assert_eq!(config.protocol, Some(WireProtocol::Swd));

        // The serial number overrides the interface.
        let config = configs.resolve(&probe("0403:6010-1:ABC"));
        assert_eq!(config.speed, Some(4000));
        assert_eq!(config.protocol, Some(WireProtocol::Swd));

        assert_eq!(configs.resolve(&probe("1111:2222")), ProbeConfig::default());
    }

    #[test]
    fn empty_serial_matches_probe_without_serial() {
        let configs = configs("[\"0403:6010:\"]\nspeed = 1000\n");

        assert_eq!(configs.resolve(&probe("0403:6010")).speed, Some(1000));
        assert_eq!(configs.resolve(&probe("0403:6010:ABC")).speed, None);
    }

    #[test]
    fn hex_serial_matches_raw_serial() {
        // "AB" in hex.
        let configs = configs("[\"0403:6010:4142\"]\nspeed = 1000\n");

        assert_eq!(configs.resolve(&probe("0403:6010:AB")).speed, Some(1000));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(is_rejected(
            r#"
            ["0403:6010"]
            sped = 4000
            "#
        ));
    }

    #[test]
    fn duplicate_selectors_are_rejected() {
        assert!(is_rejected(
            r#"
            ["1a86:55dd"]
            speed = 1

            ["1A86:55DD"]
            speed = 2
            "#
        ));
    }

    #[test]
    fn invalid_selector_is_rejected() {
        assert!(is_rejected(
            r#"
            ["not a selector"]
            speed = 1
            "#
        ));
    }

    #[test]
    fn driver_settings_replace_each_other() {
        let configs = configs(
            r#"
            ["1a86:55dd".driver]
            type = "ch347"
            reset_pin = "trst"

            ["1a86:55dd:ABC".driver]
            type = "ch347"
            "#,
        );

        let config = configs.resolve(&probe("1a86:55dd:ABC"));
        assert_eq!(
            config.driver,
            Some(DriverConfig::Ch347(Ch347Config::default()))
        );
    }

    #[test]
    fn invalid_driver_settings_are_rejected() {
        assert!(is_rejected(
            r#"
            ["0403:6010".driver]
            type = "ftdi"
            "#
        ));
        assert!(is_rejected(
            r#"
            ["1a86:55dd".driver]
            type = "ch347"
            reset_pin = "gpio8"
            "#
        ));
        assert!(is_rejected(
            r#"
            ["1a86:55dd".driver]
            type = "ch347"
            reset = "trst"
            "#
        ));
    }

    #[derive(Debug, Default)]
    struct OneProbeLister {
        list_calls: Cell<usize>,
    }

    impl ProbeLister for OneProbeLister {
        fn open(&self, _selector: &DebugProbeSelector) -> Result<Probe, DebugProbeError> {
            Ok(FakeProbe::new().into_probe())
        }

        fn list_with_access(&self, _selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
            self.list_calls.set(self.list_calls.get() + 1);
            let info = DebugProbeInfo::new(
                "Mock probe",
                0x12,
                0x23,
                Some("ABC".to_owned()),
                &CmsisDapFactory,
                None,
                false,
            );
            vec![ProbeListItem::accessible(info)]
        }
    }

    #[test]
    fn lister_applies_the_entry_with_the_serial_number() {
        let configs = configs(
            r#"
            ["0012:0023:ABC"]
            speed = 1234
            "#,
        );
        let lister = ConfiguredLister::new(OneProbeLister::default(), configs);

        let probe = lister.open(&probe("0012:0023")).unwrap();
        assert_eq!(probe.speed_khz(), 1234);
    }

    #[test]
    fn unconnected_reset_line_refuses_connect_under_reset() {
        let configs = configs(
            r#"
            ["0012:0023"]
            reset_line_connected = false
            "#,
        );
        let lister = ConfiguredLister::new(OneProbeLister::default(), configs);

        let mut probe = lister.open(&probe("0012:0023")).unwrap();
        assert!(matches!(
            probe.attach_to_unspecified_under_reset(),
            Err(probe_rs::Error::Arm(
                probe_rs::architecture::arm::ArmError::Probe(
                    DebugProbeError::ResetLineNotConnected
                )
            ))
        ));
    }

    #[test]
    fn lister_lists_only_for_entries_with_a_serial_number() {
        let configs = configs(
            r#"
            ["0012:0023"]
            speed = 1234

            ["0012:0024:ABC"]
            speed = 4321
            "#,
        );
        let lister = ConfiguredLister::new(OneProbeLister::default(), configs);

        let probe = lister.open(&probe("0012:0023")).unwrap();
        assert_eq!(probe.speed_khz(), 1234);
        assert_eq!(lister.inner.list_calls.get(), 0);
    }

    #[test]
    fn serial_number_is_needed_for_entries_with_a_serial_number() {
        let configs = configs(
            r#"
            ["0012:0023:ABC"]
            speed = 1

            ["0012:0024"]
            speed = 1
            "#,
        );

        assert!(configs.needs_serial_number(&probe("0012:0023")));
        assert!(!configs.needs_serial_number(&probe("0012:0023:ABC")));
        assert!(!configs.needs_serial_number(&probe("0012:0024")));
    }
}
