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
/// `usb-busid://BUS-PORT.PORT` selects the probe plugged into that USB port, as in
/// `usb-busid://1-1.2`. See [`UsbPort`].
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
    /// A USB probe.
    Usb(UsbSelector),
    /// The USB probe plugged into this port.
    UsbPort(UsbPort),
}

impl DebugProbeSelector {
    /// Returns whether the given USB device info matches this selector.
    pub fn matches(&self, info: &DeviceInfo) -> bool {
        match self {
            DebugProbeSelector::Usb(selector) => selector.matches(info),
            DebugProbeSelector::UsbPort(port) => UsbPort::of(info).as_ref() == Some(port),
        }
    }

    /// Check if the given probe info matches this selector.
    pub fn matches_probe(&self, info: &DebugProbeInfo) -> bool {
        match self {
            DebugProbeSelector::Usb(selector) => selector.matches_probe(info),
            DebugProbeSelector::UsbPort(port) => {
                let ProbeTransport::Usb(info) = &info.transport;
                info.usb_port.as_ref() == Some(port)
            }
        }
    }

    /// Returns whether a probe that `probe` selects matches this selector.
    pub fn matches_selector(&self, probe: &DebugProbeSelector) -> bool {
        match (self, probe) {
            (DebugProbeSelector::Usb(selector), DebugProbeSelector::Usb(probe)) => {
                selector.matches_selector(probe)
            }
            (DebugProbeSelector::UsbPort(port), DebugProbeSelector::Usb(probe)) => {
                probe.usb_port.as_ref() == Some(port)
            }
            (DebugProbeSelector::UsbPort(port), DebugProbeSelector::UsbPort(probe)) => {
                port == probe
            }
            // A port says nothing about the VID, PID or serial number.
            (DebugProbeSelector::Usb(_), DebugProbeSelector::UsbPort(_)) => false,
        }
    }
}

/// The USB port a device is plugged into: the bus, and the port number on each hub from the root
/// hub to the device, as in `1-1.2`.
///
/// Unlike the device address, it stays the same when the device enumerates again, as long as it
/// stays in the same port. On Windows, for a device without a location path, the bus is the
/// instance ID of its hub and the path is its port on that hub.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsbPort {
    /// The bus ID, without leading zeros if it is a decimal number.
    pub bus_id: String,
    /// The port numbers from the root hub to the device.
    pub ports: Vec<u8>,
}

impl UsbPort {
    /// The port that `device` is plugged into, if the platform knows it.
    pub fn of(device: &DeviceInfo) -> Option<Self> {
        let ports = device.port_chain();
        if !device.bus_id().is_empty() && !ports.is_empty() {
            return Some(Self::new(device.bus_id(), ports.to_vec()));
        }

        // Windows has no location path for some devices, but still knows the hub they are
        // plugged into and the port on it.
        #[cfg(windows)]
        if let hub = device.parent_instance_id().to_string_lossy()
            && !hub.is_empty()
            && let Ok(port) = u8::try_from(device.port_number())
            && port != 0
        {
            return Some(Self::new(&hub, vec![port]));
        }

        None
    }

    fn new(bus_id: &str, ports: Vec<u8>) -> Self {
        // Linux numbers buses `001`, but names ports `1-1.2`.
        let bus_id = if !bus_id.is_empty() && bus_id.bytes().all(|b| b.is_ascii_digit()) {
            let trimmed = bus_id.trim_start_matches('0');
            if trimmed.is_empty() { "0" } else { trimmed }
        } else {
            bus_id
        };
        Self {
            bus_id: bus_id.to_string(),
            ports,
        }
    }
}

impl fmt::Display for UsbPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-", self.bus_id)?;
        for (i, port) in self.ports.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{port}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for UsbPort {
    type Err = DebugProbeSelectorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // The ports come after the last dash; a Windows bus ID may contain dashes.
        let (bus_id, ports) = s
            .rsplit_once('-')
            .ok_or(DebugProbeSelectorParseError::Format)?;
        if bus_id.is_empty() {
            return Err(DebugProbeSelectorParseError::Format);
        }
        let ports = ports
            .split('.')
            .map(str::parse)
            .collect::<Result<Vec<u8>, _>>()?;
        Ok(Self::new(bus_id, ports))
    }
}

/// Selects a USB probe by its vendor and product ID, and optionally its interface, serial number
/// and USB port.
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
    /// The USB port of the probe, if known. Parsing a `VID:PID` string never sets it.
    ///
    /// Tells identical probes (same VID/PID and serial number) apart.
    ///
    /// See [`UsbProbeInfo::usb_port`](crate::probe::UsbProbeInfo::usb_port).
    pub usb_port: Option<UsbPort>,
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

        if self.usb_port.is_some() && self.usb_port != UsbPort::of(info) {
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
        let ProbeTransport::Usb(info) = &info.transport;
        if self.usb_port.is_some() && info.usb_port != self.usb_port {
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

impl std::str::FromStr for DebugProbeSelector {
    type Err = DebugProbeSelectorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // A scheme comes before the first colon; a serial number may contain `://`.
        let s = match s.split_once("://") {
            Some(("usb", rest)) => rest,
            Some(("usb-busid", port)) => return Ok(DebugProbeSelector::UsbPort(port.parse()?)),
            Some((scheme, _)) if !scheme.contains(':') => {
                return Err(DebugProbeSelectorParseError::UnknownScheme(
                    scheme.to_string(),
                ));
            }
            _ => s,
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
            usb_port: None,
        }))
    }
}

impl From<DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: DebugProbeInfo) -> Self {
        let ProbeTransport::Usb(selector) = selector.transport;
        DebugProbeSelector::Usb(UsbSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
            interface: selector.interface,
            usb_port: selector.usb_port,
        })
    }
}

impl From<&DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: &DebugProbeInfo) -> Self {
        let ProbeTransport::Usb(selector) = &selector.transport;
        DebugProbeSelector::Usb(UsbSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number.clone(),
            interface: selector.interface,
            usb_port: selector.usb_port.clone(),
        })
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
            DebugProbeSelector::UsbPort(port) => write!(f, "usb-busid://{port}"),
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

    /// Unknown probe selector scheme `{0}://`. The schemes are `usb://` and `usb-busid://`.
    UnknownScheme(String),
}

#[cfg(test)]
mod test {
    use crate::probe::{DebugProbeSelector, UsbPort, UsbSelector};

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
    fn usb_busid_selects_a_port() {
        let selector: DebugProbeSelector = "usb-busid://001-1.2".parse().unwrap();
        let port = UsbPort {
            bus_id: "1".to_string(),
            ports: vec![1, 2],
        };
        assert_eq!(selector, DebugProbeSelector::UsbPort(port));
        assert_eq!(selector.to_string(), "usb-busid://1-1.2");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(matches!(
            "tcp://localhost:4441".parse::<DebugProbeSelector>(),
            Err(super::DebugProbeSelectorParseError::UnknownScheme(scheme)) if scheme == "tcp"
        ));
    }

    #[test]
    fn display_without_interface_unchanged() {
        // Selectors without interface must continue to display without a dash.
        let selector = usb("0403:6010:ABCD1234");
        assert_eq!(selector.interface, None);
        assert_eq!(selector.to_string(), "0403:6010:ABCD1234");
    }
}
