//! The probe table of the configuration file: settings for each probe.

use std::collections::HashMap;

use anyhow::Context as _;
use probe_rs::probe::{
    DebugProbeError, DebugProbeSelector, Probe, WireProtocol,
    list::{ProbeListItem, ProbeLister},
    target_serial::{LineState, SerialSettings, TargetSerial, UsbCdcSerial},
    usb_util::UsbSerialFilter,
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
/// [probes."303a:1001".serial]
/// rts = "low"
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
    /// The serial port to the target.
    pub serial: SerialConfig,
}

/// Settings of the serial port to the target. A `None` field keeps the driver default.
///
/// The machine with the probe owns these settings. In remote mode, the client can only ask for
/// the port and its baud rate.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SerialConfig {
    /// `false` never opens the port.
    pub enabled: Option<bool>,
    pub baud: Option<u32>,
    pub dtr: Option<LineState>,
    pub rts: Option<LineState>,
    /// Use this USB serial port instead of the port of the probe: `VID:PID[:serial]`.
    pub usb: Option<String>,
    /// Use the USB serial port with this OS name instead of the port of the probe.
    pub port: Option<String>,
}

impl SerialConfig {
    /// Sets the fields of `self` that `other` sets.
    fn merge(&mut self, other: &SerialConfig) {
        let other = other.clone();
        self.enabled = other.enabled.or(self.enabled);
        self.baud = other.baud.or(self.baud);
        self.dtr = other.dtr.or(self.dtr);
        self.rts = other.rts.or(self.rts);
        // A replacement port is one setting: `usb` and `port` replace each other.
        if other.usb.is_some() || other.port.is_some() {
            self.usb = other.usb;
            self.port = other.port;
        }
    }

    fn apply(&self, settings: &mut SerialSettings) {
        if let Some(baud) = self.baud {
            settings.baud = baud;
        }
        if let Some(dtr) = self.dtr {
            settings.dtr = dtr;
        }
        if let Some(rts) = self.rts {
            settings.rts = rts;
        }
    }

    /// The filter of the replacement port, if the config names one.
    fn replacement(&self) -> anyhow::Result<Option<UsbSerialFilter>> {
        if let Some(port) = &self.port {
            return Ok(Some(UsbSerialFilter {
                port_name: Some(port.clone()),
                ..Default::default()
            }));
        }
        let Some(usb) = &self.usb else {
            return Ok(None);
        };
        let selector = usb
            .parse::<DebugProbeSelector>()
            .with_context(|| format!("Invalid serial port `{usb}`"))?;
        Ok(Some(UsbSerialFilter {
            vendor_id: Some(selector.vendor_id),
            product_id: Some(selector.product_id),
            serial_number: selector.serial_number,
            interface: selector.interface,
            port_name: None,
        }))
    }

    /// Takes the serial port to the target from `probe`, or the replacement port, and returns it
    /// with its settings. Returns `None` if there is no port or the config disables it.
    #[expect(
        dead_code,
        reason = "The RPC serial stream uses this in the next change."
    )]
    pub fn target_serial(
        &self,
        probe: &mut Probe,
    ) -> anyhow::Result<Option<(Box<dyn TargetSerial>, SerialSettings)>> {
        if self.enabled == Some(false) {
            return Ok(None);
        }

        let serial: Box<dyn TargetSerial> = match self.replacement()? {
            Some(filter) => Box::new(UsbCdcSerial::new(filter, SerialSettings::default())),
            None => match probe.target_serial() {
                Some(serial) => serial,
                None => return Ok(None),
            },
        };

        let mut settings = serial.default_settings();
        self.apply(&mut settings);
        Ok(Some((serial, settings)))
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
        self.serial.merge(&other.serial);
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

    #[test]
    fn serial_settings_merge_and_apply() {
        let configs = configs(
            r#"
            ["303a:1001".serial]
            baud = 9600
            dtr = "low"
            usb = "1a86:7523"

            ["303a:1001:ABC".serial]
            dtr = "high"
            port = "COM7"
            "#,
        );

        let serial = configs.resolve(&probe("303a:1001:ABC")).serial;
        let mut settings = SerialSettings::default();
        serial.apply(&mut settings);
        assert_eq!(settings.baud, 9600);
        assert_eq!(settings.dtr, LineState::High);
        assert_eq!(settings.rts, LineState::Keep);

        // The specific entry replaces the replacement port of the general entry.
        let filter = serial.replacement().unwrap().unwrap();
        assert_eq!(filter.port_name.as_deref(), Some("COM7"));
        assert_eq!(filter.vendor_id, None);

        let serial = configs.resolve(&probe("303a:1001:OTHER")).serial;
        let filter = serial.replacement().unwrap().unwrap();
        assert_eq!(filter.vendor_id, Some(0x1a86));
        assert_eq!(filter.product_id, Some(0x7523));
    }
}
