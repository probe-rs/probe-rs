use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::probe::DebugProbeInfo;

use nusb::DeviceInfo;

/// A struct to describe the way a probe should be selected.
///
/// Construct this from a set of info or from a string. The
/// string has to be in the format "VID:PID-INTERFACE:SERIALNUMBER".
///
/// The interface number and serial number are optional, and VID and PID are
/// parsed as hexadecimal numbers.
///
/// If SERIALNUMBER exists (i.e. the selector contains a second color) and is empty,
/// probe-rs will select probes that have no serial number, or where the serial number is empty.
///
/// ## Example:
///
/// ```
/// use std::str::FromStr;
/// let selector: probe_rs::probe::DebugProbeSelector = "1942:1337:SERIAL".parse().unwrap();
///
/// assert_eq!(selector.vendor_id, 0x1942);
/// assert_eq!(selector.product_id, 0x1337);
/// ```
///
/// With interface number:
/// ```
/// use std::str::FromStr;
/// let selector: probe_rs::probe::DebugProbeSelector = "1942:1337-3".parse().unwrap();
///
/// assert_eq!(selector.vendor_id, 0x1942);
/// assert_eq!(selector.product_id, 0x1337);
/// assert_eq!(selector.interface, Some(3));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DebugProbeSelector {
    /// The USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The USB interface of the debug probe to be used.
    pub interface: Option<u8>,
    /// The serial number of the debug probe to be used.
    pub serial_number: Option<String>,
}

impl DebugProbeSelector {
    pub(crate) fn matches(&self, info: &DeviceInfo) -> bool {
        if self.interface.is_some() {
            info.interfaces().any(|iface| {
                self.match_probe_selector(
                    info.vendor_id(),
                    info.product_id(),
                    Some(iface.interface_number()),
                    info.serial_number(),
                )
            })
        } else {
            self.match_probe_selector(
                info.vendor_id(),
                info.product_id(),
                None,
                info.serial_number(),
            )
        }
    }

    /// Check if the given probe info matches this selector.
    pub fn matches_probe(&self, info: &DebugProbeInfo) -> bool {
        self.match_probe_selector(
            info.vendor_id,
            info.product_id,
            info.interface,
            info.serial_number.as_deref(),
        )
    }

    fn match_probe_selector(
        &self,
        vendor_id: u16,
        product_id: u16,
        interface: Option<u8>,
        serial_number: Option<&str>,
    ) -> bool {
        tracing::trace!(
            "Matching probe selector:\nVendor ID: {vendor_id:04x} == {:04x}\nProduct ID: {product_id:04x} = {:04x}\nInterface: {interface:?} == {:?}\nSerial Number: {serial_number:?} == {:?}",
            self.vendor_id,
            self.product_id,
            self.interface,
            self.serial_number
        );

        vendor_id == self.vendor_id
            && product_id == self.product_id
            && self
                .interface
                .map(|iface| interface == Some(iface))
                .unwrap_or(true) // USB interface not specified by user
            && self
                .serial_number
                .as_ref()
                .map(|s| {
                    if let Some(serial_number) = serial_number {
                        serial_number == s
                    } else {
                        // Match probes without serial number when the
                        // selector has a third, empty part ("VID:PID:")
                        s.is_empty()
                    }
                })
                .unwrap_or(true)
    }
}

impl std::str::FromStr for DebugProbeSelector {
    type Err = DebugProbeSelectorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split into at most 3 parts: VID, PID, Serial.
        // We limit the number of splits to allow for colons in the
        // serial number (EspJtag uses MAC address)
        let mut split = s.splitn(3, ':');

        let vendor_id = split.next().unwrap(); // First split is always successful
        let mut product_id = split.next().ok_or(DebugProbeSelectorParseError::Format)?;
        let interface = if let Some((id, iface)) = product_id.split_once("-") {
            product_id = id;
            // Matches probes without interface where the selector has minus but no interface number ("VID:PID-")
            if iface.is_empty() {
                Ok(None)
            } else {
                iface.parse::<u8>().map(Some)
            }
        } else {
            Ok(None)
        }?;
        let serial_number = split.next().map(|s| s.to_string());

        Ok(DebugProbeSelector {
            vendor_id: u16::from_str_radix(vendor_id, 16)?,
            product_id: u16::from_str_radix(product_id, 16)?,
            serial_number,
            interface,
        })
    }
}

impl From<DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: DebugProbeInfo) -> Self {
        DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
        }
    }
}

impl From<&DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: &DebugProbeInfo) -> Self {
        DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number.clone(),
            interface: selector.interface,
        }
    }
}

impl fmt::Display for DebugProbeSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor_id, self.product_id)?;
        if let Some(ref sn) = self.serial_number {
            write!(f, ":{sn}")?;
        }
        Ok(())
    }
}

impl Serialize for DebugProbeSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'a> Deserialize<'a> for DebugProbeSelector {
    fn deserialize<D>(deserializer: D) -> Result<DebugProbeSelector, D::Error>
    where
        D: Deserializer<'a>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// An error which can occur while parsing a [`DebugProbeSelector`].
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum DebugProbeSelectorParseError {
    /// Could not parse VID or PID: {0}
    ParseInt(#[from] std::num::ParseIntError),

    /// The format of the selector is invalid. Please use a string in the form `VID:PID<-Interface>:<Serial>`, where Serial  and Interface are optional.
    Format,
}

#[cfg(test)]
mod test {
    use crate::probe::DebugProbeSelector;

    #[test]
    fn test_parsing_many_colons() {
        let selector: DebugProbeSelector = "303a:1001:DC:DA:0C:D3:FE:D8".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(
            selector.serial_number,
            Some("DC:DA:0C:D3:FE:D8".to_string())
        );
    }

    #[test]
    fn missing_serial_is_none() {
        let selector: DebugProbeSelector = "303a:1001".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(selector.serial_number, None);

        let matches = selector.match_probe_selector(0x303a, 0x1001, None, None);
        let matches_with_serial =
            selector.match_probe_selector(0x303a, 0x1001, None, Some("serial"));
        assert!(matches);
        assert!(matches_with_serial);
    }

    #[test]
    fn empty_serial_is_some() {
        let selector: DebugProbeSelector = "303a:1001:".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(selector.serial_number, Some(String::new()));

        let matches = selector.match_probe_selector(0x303a, 0x1001, None, None);
        let matches_with_serial =
            selector.match_probe_selector(0x303a, 0x1001, None, Some("serial"));
        assert!(matches);
        assert!(!matches_with_serial);
    }

    #[test]
    fn missing_interface_is_none() {
        let selector: DebugProbeSelector = "303a:1001".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(selector.interface, None);

        let matches = selector.match_probe_selector(0x303a, 0x1001, None, None);
        let matches_with_interface = selector.match_probe_selector(0x303a, 0x1001, Some(0), None);
        assert!(matches);
        assert!(matches_with_interface);
    }

    #[test]
    fn empty_interface_is_none() {
        let selector: DebugProbeSelector = "303a:1001-".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(selector.interface, None);

        let matches = selector.match_probe_selector(0x303a, 0x1001, None, None);
        let matches_with_interface = selector.match_probe_selector(0x303a, 0x1001, Some(0), None);
        assert!(matches);
        assert!(matches_with_interface);
    }

    #[test]
    fn set_interface_matches() {
        let selector: DebugProbeSelector = "303a:1001-0".parse().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(selector.interface, Some(0));

        let no_match = selector.match_probe_selector(0x303a, 0x1001, None, None);
        let matches_with_interface = selector.match_probe_selector(0x303a, 0x1001, Some(0), None);
        let no_match_with_wrong_interface =
            selector.match_probe_selector(0x303a, 0x1001, Some(1), None);
        assert!(!no_match);
        assert!(matches_with_interface);
        assert!(!no_match_with_wrong_interface);
    }
}
