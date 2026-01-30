//! Probe driver implementations

use crate::probe::ProbeFactory;

pub mod blackmagic;
pub mod ch347usbjtag;
pub mod cmsisdap;
pub mod espusbjtag;
pub mod ftdi;
pub mod glasgow;
pub mod jlink;
pub mod sifliuart;
pub mod stlink;
pub mod wlink;

mod usb_util;

/// Used to log warnings when the measured target voltage is
/// lower than 1.4V, if at all measurable.
const LOW_TARGET_VOLTAGE_WARNING_THRESHOLD: f32 = 1.4;

pub(crate) const DRIVERS: &[&'static dyn ProbeFactory] = &[
    &blackmagic::BlackMagicProbeFactory,
    &cmsisdap::CmsisDapFactory,
    &ftdi::FtdiProbeFactory,
    &stlink::StLinkFactory,
    &jlink::JLinkFactory,
    &espusbjtag::EspUsbJtagFactory,
    &wlink::WchLinkFactory,
    &sifliuart::SifliUartFactory,
    &glasgow::GlasgowFactory,
    &ch347usbjtag::Ch347UsbJtagFactory,
];
