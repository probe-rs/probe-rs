use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::probe::{DebugProbeInfo, ProbeTransport, usb_util::to_hex};

use nusb::DeviceInfo;

/// Describes the way a probe should be selected.
///
/// Construct this from a set of info or from a string. The
/// string has to be in the format "VID:PID-INTERFACE:SERIALNUMBER".
///
/// The interface number and serial number are optional, and VID and PID are
/// parsed as hexadecimal numbers. The string may start with `usb://`.
///
/// `tcp://DRIVER@ADDRESS` selects a probe reached over TCP, as in `tcp://cmsis-dap@192.168.1.50`.
/// See [`TcpAddress`].
///
/// If SERIALNUMBER exists (i.e. the selector contains a second color) and is empty,
/// probe-rs will select probes that have no serial number, or where the serial number is empty.
///
/// ## Example:
///
/// ```
/// use std::str::FromStr;
/// use probe_rs::probe::DebugProbeSelector;
/// let Ok(DebugProbeSelector::Usb(selector)) = "1942:1337:SERIAL".parse() else {
///     unreachable!()
/// };
///
/// assert_eq!(selector.vendor_id, 0x1942);
/// assert_eq!(selector.product_id, 0x1337);
/// ```
///
/// With interface number:
/// ```
/// use std::str::FromStr;
/// use probe_rs::probe::DebugProbeSelector;
/// let Ok(DebugProbeSelector::Usb(selector)) = "1942:1337-3".parse() else {
///     unreachable!()
/// };
///
/// assert_eq!(selector.vendor_id, 0x1942);
/// assert_eq!(selector.product_id, 0x1337);
/// assert_eq!(selector.interface, Some(3));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DebugProbeSelector {
    /// A USB probe, selected by its VID and PID.
    ///
    /// Some probes that are not on USB, such as the Linux GPIO and SPI probes and some probes
    /// reached over TCP, are selected through this variant too for now: with a placeholder VID and
    /// PID, and their address in the serial number.
    Usb(UsbSelector),
    /// A probe reached over TCP.
    Tcp(TcpAddress),
}

impl DebugProbeSelector {
    /// Returns whether the given USB device info matches this selector.
    pub fn matches(&self, info: &DeviceInfo) -> bool {
        match self {
            DebugProbeSelector::Usb(selector) => selector.matches(info),
            DebugProbeSelector::Tcp(_) => false,
        }
    }

    /// Check if the given probe info matches this selector.
    pub fn matches_probe(&self, info: &DebugProbeInfo) -> bool {
        match self {
            DebugProbeSelector::Usb(selector) => selector.matches_probe(info),
            DebugProbeSelector::Tcp(address) => {
                matches!(&info.transport, ProbeTransport::Tcp(info) if info == address)
            }
        }
    }

    /// Returns whether a probe that `probe` selects matches this selector.
    pub fn matches_selector(&self, probe: &DebugProbeSelector) -> bool {
        match (self, probe) {
            (DebugProbeSelector::Usb(selector), DebugProbeSelector::Usb(probe)) => {
                selector.matches_selector(probe)
            }
            (DebugProbeSelector::Tcp(address), DebugProbeSelector::Tcp(probe)) => address == probe,
            _ => false,
        }
    }
}

/// A probe reached over TCP: the driver that speaks its protocol, and its address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TcpAddress {
    /// The name of the driver, as in `cmsis-dap`.
    pub driver: String,
    /// The address as the driver takes it, such as `host` or `host:port`.
    pub address: String,
}

impl fmt::Display for TcpAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tcp://{}@{}", self.driver, self.address)
    }
}

/// Selects a USB probe by its vendor and product ID, and optionally its interface, serial number
/// and USB location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsbSelector {
    /// The USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The USB interface of the debug probe to be used.
    pub interface: Option<u8>,
    /// The serial number of the debug probe to be used.
    pub serial_number: Option<String>,
    /// USB bus ID and device address, if known. Parsing a string never sets it.
    ///
    /// See [`UsbProbeInfo::usb_location`](crate::probe::UsbProbeInfo::usb_location).
    pub usb_location: Option<(String, u8)>,
}

impl UsbSelector {
    /// Returns whether the given USB device info matches this selector.
    pub fn matches(&self, info: &DeviceInfo) -> bool {
        fn matches_with_interface(
            selector: &UsbSelector,
            info: &DeviceInfo,
            interface: Option<u8>,
        ) -> bool {
            selector.match_probe_selector(
                info.vendor_id(),
                info.product_id(),
                interface,
                info.serial_number(),
            )
        }

        if let Some((bus_id, device_address)) = &self.usb_location
            && (bus_id.as_str(), *device_address) != (info.bus_id(), info.device_address())
        {
            return false;
        }

        if self.interface.is_some() {
            info.interfaces()
                .any(|iface| matches_with_interface(self, info, Some(iface.interface_number())))
        } else {
            matches_with_interface(self, info, None)
        }
    }

    /// Check if the given probe info matches this selector.
    pub fn matches_probe(&self, info: &DebugProbeInfo) -> bool {
        let ProbeTransport::Usb(info) = &info.transport else {
            return false;
        };
        if self.usb_location.is_some() && info.usb_location != self.usb_location {
            return false;
        }

        self.match_probe_selector(
            info.vendor_id,
            info.product_id,
            info.interface,
            info.serial_number.as_deref(),
        )
    }

    /// Returns whether a probe that `probe` selects matches this selector.
    pub fn matches_selector(&self, probe: &UsbSelector) -> bool {
        self.match_probe_selector(
            probe.vendor_id,
            probe.product_id,
            probe.interface,
            probe.serial_number.as_deref(),
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
                        serial_number == s || to_hex(serial_number).as_str() == s
                    } else {
                        // Match probes without serial number when the
                        // selector has a third, empty part ("VID:PID:")
                        s.is_empty()
                    }
                })
                .unwrap_or(true)
    }
}

/// Splits a leading `scheme://` off `s`, if it has one.
///
/// A scheme is a letter followed by letters, digits, `+`, `-` and `.`, as in a URI. So a `://` in
/// the serial number of `VID:PID:SERIAL` does not start a scheme: its prefix contains colons.
fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = s.split_once("://")?;
    let mut chars = scheme.chars();
    let is_scheme = chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme.then_some((scheme, rest))
}

impl std::str::FromStr for DebugProbeSelector {
    type Err = DebugProbeSelectorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = match split_scheme(s) {
            Some(("usb", rest)) => rest,
            Some(("tcp", address)) => {
                return match address.split_once('@') {
                    Some((driver, address)) if !driver.is_empty() && !address.is_empty() => {
                        Ok(DebugProbeSelector::Tcp(TcpAddress {
                            driver: driver.to_string(),
                            address: address.to_string(),
                        }))
                    }
                    _ => Err(DebugProbeSelectorParseError::Format),
                };
            }
            Some((scheme, _)) => {
                return Err(DebugProbeSelectorParseError::UnknownScheme(
                    scheme.to_string(),
                ));
            }
            None => s,
        };

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

        Ok(DebugProbeSelector::Usb(UsbSelector {
            vendor_id: u16::from_str_radix(vendor_id, 16)?,
            product_id: u16::from_str_radix(product_id, 16)?,
            serial_number,
            interface,
            usb_location: None,
        }))
    }
}

impl From<DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: DebugProbeInfo) -> Self {
        match selector.transport {
            ProbeTransport::Usb(selector) => DebugProbeSelector::Usb(UsbSelector {
                vendor_id: selector.vendor_id,
                product_id: selector.product_id,
                serial_number: selector.serial_number,
                interface: selector.interface,
                usb_location: selector.usb_location,
            }),
            ProbeTransport::Tcp(address) => DebugProbeSelector::Tcp(address),
        }
    }
}

impl From<&DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: &DebugProbeInfo) -> Self {
        match &selector.transport {
            ProbeTransport::Usb(selector) => DebugProbeSelector::Usb(UsbSelector {
                vendor_id: selector.vendor_id,
                product_id: selector.product_id,
                serial_number: selector.serial_number.clone(),
                interface: selector.interface,
                usb_location: selector.usb_location.clone(),
            }),
            ProbeTransport::Tcp(address) => DebugProbeSelector::Tcp(address.clone()),
        }
    }
}

impl From<UsbSelector> for DebugProbeSelector {
    fn from(selector: UsbSelector) -> Self {
        DebugProbeSelector::Usb(selector)
    }
}

impl fmt::Display for DebugProbeSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DebugProbeSelector::Usb(selector) => selector.fmt(f),
            DebugProbeSelector::Tcp(address) => address.fmt(f),
        }
    }
}

impl fmt::Display for UsbSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor_id, self.product_id)?;
        if let Some(interface) = self.interface {
            write!(f, "-{interface}")?;
        }
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

    /// Unknown probe selector scheme `{0}://`. The schemes are `usb://` and `tcp://`.
    UnknownScheme(String),
}

#[cfg(test)]
mod test {
    use crate::probe::{DebugProbeSelector, TcpAddress, UsbSelector};

    fn usb(selector: &str) -> UsbSelector {
        let Ok(DebugProbeSelector::Usb(selector)) = selector.parse() else {
            panic!("{selector} is not a USB selector");
        };
        selector
    }

    #[test]
    fn test_parsing_many_colons() {
        let selector = usb("303a:1001:DC:DA:0C:D3:FE:D8");

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(
            selector.serial_number,
            Some("DC:DA:0C:D3:FE:D8".to_string())
        );
    }

    #[test]
    fn missing_serial_is_none() {
        let selector = usb("303a:1001");

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
        let selector = usb("303a:1001:");

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
        let selector = usb("303a:1001");

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
        let selector = usb("303a:1001-");

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
        let selector = usb("303a:1001-0");

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

    #[test]
    fn display_round_trips_with_interface() {
        let selector = usb("0403:6010-1");
        assert_eq!(selector.interface, Some(1));
        assert_eq!(selector.to_string(), "0403:6010-1");

        // Display output must parse back to an equivalent selector.
        let reparsed = usb(&selector.to_string());
        assert_eq!(reparsed.vendor_id, selector.vendor_id);
        assert_eq!(reparsed.product_id, selector.product_id);
        assert_eq!(reparsed.interface, selector.interface);
        assert_eq!(reparsed.serial_number, selector.serial_number);
    }

    #[test]
    fn display_round_trips_with_interface_and_serial() {
        let selector = usb("0403:6010-1:ABCD1234");
        assert_eq!(selector.interface, Some(1));
        assert_eq!(selector.serial_number, Some("ABCD1234".to_string()));
        assert_eq!(selector.to_string(), "0403:6010-1:ABCD1234");

        let reparsed = usb(&selector.to_string());
        assert_eq!(reparsed.vendor_id, selector.vendor_id);
        assert_eq!(reparsed.product_id, selector.product_id);
        assert_eq!(reparsed.interface, selector.interface);
        assert_eq!(reparsed.serial_number, selector.serial_number);
    }

    #[test]
    fn usb_scheme_is_optional() {
        assert_eq!(usb("usb://0403:6010-1:A://B"), usb("0403:6010-1:A://B"));
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(matches!(
            "serial:///dev/ttyACM0".parse::<DebugProbeSelector>(),
            Err(super::DebugProbeSelectorParseError::UnknownScheme(scheme)) if scheme == "serial"
        ));
    }

    #[test]
    fn tcp_names_the_driver_and_the_address() {
        let selector: DebugProbeSelector = "tcp://cmsis-dap@[::1]:4441".parse().unwrap();
        let address = TcpAddress {
            driver: "cmsis-dap".to_string(),
            address: "[::1]:4441".to_string(),
        };
        assert_eq!(selector, DebugProbeSelector::Tcp(address));
        assert_eq!(selector.to_string(), "tcp://cmsis-dap@[::1]:4441");
        assert!(
            "tcp://localhost:4441"
                .parse::<DebugProbeSelector>()
                .is_err()
        );
    }

    #[test]
    fn display_without_interface_unchanged() {
        // Selectors without interface must continue to display without a dash.
        let selector = usb("0403:6010:ABCD1234");
        assert_eq!(selector.interface, None);
        assert_eq!(selector.to_string(), "0403:6010:ABCD1234");
    }
}
