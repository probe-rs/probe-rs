/// List of target interfaces.
///
/// Note that this library might not support all of them, despite listing them here.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Interface {
    /// JTAG interface (IEEE 1149.1). Supported by most J-Link probes (some embedded J-Links
    /// might only support SWD).
    Jtag = 0,
    /// SWD interface (Serial Wire Debug), used by most Cortex-M chips, and supported by almost
    /// all J-Link probes.
    Swd = 1,
    /// Background Debug Mode 3, a single-wire debug interface used on some NXP microcontrollers.
    Bdm3 = 2,
    /// FINE, a two-wire debugging interface used by Renesas RX MCUs.
    ///
    /// **Note**: due to a bug, attempting to select FINE with `JLink::select_interface()` will
    /// currently hang the probe.
    // FIXME: There's a curious bug that hangs the probe when selecting the FINE interface.
    // Specifically, the probe never sends back the previous interface after it receives the
    // `c7 03` SELECT_IF cmd, even though the normal J-Link software also just sends `c7 03`
    // and gets back the right response.
    Fine = 3,
    /// In-Circuit System Programming (ICSP) interface of PIC32 chips.
    Pic32Icsp = 4,
    /// Serial Peripheral Interface (for SPI Flash programming).
    Spi = 5,
    /// Silicon Labs' 2-wire debug interface.
    C2 = 6,
    /// [cJTAG], or compact JTAG, as specified in IEEE 1149.7.
    ///
    /// [cJTAG]: https://wiki.segger.com/J-Link_cJTAG_specifics.
    CJtag = 7,
    /// 2-wire debugging interface used by Microchip's IS208x MCUs.
    Mc2WireJtag = 10,
}

impl Interface {
    const fn next(self) -> Option<Self> {
        let next = match self {
            Self::Jtag => Self::Swd,
            Self::Swd => Self::Bdm3,
            Self::Bdm3 => Self::Fine,
            Self::Fine => Self::Pic32Icsp,
            Self::Pic32Icsp => Self::Spi,
            Self::Spi => Self::C2,
            Self::C2 => Self::CJtag,
            Self::CJtag => Self::Mc2WireJtag,
            Self::Mc2WireJtag => return None,
        };
        Some(next)
    }

    fn mask(self) -> u32 {
        1 << self as u32
    }

    fn all_mask() -> u32 {
        InterfaceIter::new().fold(0, |mask, interface| mask | interface.mask())
    }
}

/// Iterator over supported [`Interface`]s.
#[derive(Debug)]
pub struct InterfaceIter {
    current: Interface,
}

impl InterfaceIter {
    pub fn new() -> Self {
        Self {
            current: Interface::Jtag,
        }
    }
}

impl Iterator for InterfaceIter {
    type Item = Interface;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(next) = self.current.next() {
            self.current = next;
            Some(next)
        } else {
            None
        }
    }
}

/// A set of supported target interfaces.
///
/// This implements `IntoIterator`, so you can call `.into_iter()` to iterate over the contained
/// [`Interface`]s.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Interfaces(u32);

impl Interfaces {
    pub(crate) fn from_bits_warn(raw: u32) -> Self {
        let flags = raw & Interface::all_mask();
        if flags != raw {
            tracing::debug!(
                "unknown bits in interface mask: {raw:#010x} truncated to {flags:#010x}"
            );
        }
        Self(flags)
    }

    pub(crate) fn single(interface: Interface) -> Self {
        Self(interface.mask())
    }

    /// Returns whether `interface` is contained in `self`.
    pub fn contains(&self, interface: Interface) -> bool {
        self.0 & interface.mask() != 0
    }
}

impl IntoIterator for Interfaces {
    type Item = Interface;
    type IntoIter = InterfacesIter;

    fn into_iter(self) -> Self::IntoIter {
        InterfacesIter {
            interfaces: self,
            current: InterfaceIter::new(),
        }
    }
}

/// Iterator over supported [`Interface`]s.
pub struct InterfacesIter {
    interfaces: Interfaces,
    current: InterfaceIter,
}

impl Iterator for InterfacesIter {
    type Item = Interface;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(current) = self.current.next() {
            if self.interfaces.contains(current) {
                return Some(current);
            }
        }
        None
    }
}
