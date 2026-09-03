//! The CH347 device: enumeration, open, protocol and speed state, and lifecycle.

use std::time::Duration;

use bitvec::vec::BitVec;
use nusb::{
    DeviceInfo, MaybeFuture,
    descriptors::TransferType,
    transfer::{Bulk, Direction, In, Out},
};

use crate::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError, WireProtocol,
    list::{ProbeListItem, usb_probe_accessibility},
};

use super::Ch347Factory;
use super::capabilities::{Capabilities, Clock, Pack, Variant, jtag_clock};
use super::jtag::JtagCycle;
use super::swd::{SwdClock, WAIT_BACKOFF};
use super::transport::{Ch347Error, Transport, UsbTransport};

const VID_WCH: u16 = 0x1A86;
const PID_CH347F: u16 = 0x55DE;
const PID_CH347T: u16 = 0x55DD;
const CH34X_VID_PID: [(u16, u16); 3] = [
    (VID_WCH, PID_CH347F),
    (VID_WCH, PID_CH347T),
    (VID_WCH, 0x55E8),
];
const CH347F_INTERFACE_NUM: u8 = 4;

const CMD_SWD_INIT: u8 = 0xE5;

const DEFAULT_JTAG_KHZ: u32 = 15000;

fn is_ch34x_device(device: &DeviceInfo) -> bool {
    CH34X_VID_PID.contains(&(device.vendor_id(), device.product_id()))
}

/// One CH347 with its vendor interface claimed.
#[derive(Debug)]
pub(crate) struct Ch347Device {
    pub(super) transport: Box<dyn Transport>,
    pub(super) capabilities: Capabilities,
    pub(super) protocol: WireProtocol,
    /// Known once the first JTAG init has run.
    pub(super) pack: Option<Pack>,
    pub(super) requested_khz: Option<u32>,
    pub(super) clock: Option<Clock>,
    pub(super) out_of_sync: bool,
    pub(super) jtag_queue: Vec<JtagCycle>,
    pub(super) jtag_captured: BitVec,
    pub(super) wait_backoff: Duration,
}

/// The vendor-class interface with a bulk pipe in each direction, as (number, out, in).
fn vendor_interface(device: &nusb::Device) -> Option<(u8, u8, u8)> {
    let config = device.active_configuration().ok()?;
    config.interfaces().find_map(|group| {
        let alt = group.first_alt_setting();
        if alt.class() != 0xFF {
            return None;
        }
        let bulk = |direction| {
            alt.endpoints()
                .find(|ep| ep.transfer_type() == TransferType::Bulk && ep.direction() == direction)
                .map(|ep| ep.address())
        };
        Some((
            group.interface_number(),
            bulk(Direction::Out)?,
            bulk(Direction::In)?,
        ))
    })
}

impl Ch347Device {
    pub(crate) fn new_from_selector(
        selector: &DebugProbeSelector,
    ) -> Result<Self, ProbeCreationError> {
        let usb = |e: nusb::Error| ProbeCreationError::Usb(e.into());
        let devices = nusb::list_devices().wait().map_err(usb)?;
        let device = devices
            .filter(is_ch34x_device)
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;

        let handle = device.open().wait().map_err(usb)?;

        let (interface_number, out, inp) =
            vendor_interface(&handle).ok_or(Ch347Error::NoInterface)?;
        let variant = match device.product_id() {
            PID_CH347F => Variant::Ch347F,
            PID_CH347T => Variant::Ch347T,
            _ if interface_number == CH347F_INTERFACE_NUM => Variant::Ch347F,
            _ => Variant::Ch347T,
        };
        let capabilities = Capabilities {
            variant,
            firmware: device.device_version(),
        };
        tracing::info!(
            "{variant:?} firmware {}.{:02x} on interface {interface_number}",
            capabilities.firmware >> 8,
            capabilities.firmware & 0xFF
        );

        let interface = handle
            .detach_and_claim_interface(interface_number)
            .wait()
            .map_err(usb)?;
        let out = interface.endpoint::<Bulk, Out>(out).map_err(usb)?;
        let inp = interface.endpoint::<Bulk, In>(inp).map_err(usb)?;
        let transport = UsbTransport::open(interface, out, inp);

        Ok(Self::new(Box::new(transport), capabilities))
    }

    pub(crate) fn new(transport: Box<dyn Transport>, capabilities: Capabilities) -> Self {
        Self {
            transport,
            capabilities,
            protocol: WireProtocol::Jtag,
            pack: None,
            requested_khz: None,
            clock: None,
            out_of_sync: false,
            jtag_queue: Vec::new(),
            jtag_captured: BitVec::new(),
            wait_backoff: WAIT_BACKOFF,
        }
    }

    pub(crate) fn protocol(&self) -> WireProtocol {
        self.protocol
    }

    fn default_khz(&self) -> u32 {
        match self.protocol {
            WireProtocol::Jtag => DEFAULT_JTAG_KHZ,
            WireProtocol::Swd => SwdClock::fastest(self.capabilities.swd_5mhz()).khz(),
        }
    }

    fn resolve(&mut self, khz: u32) -> Result<Clock, DebugProbeError> {
        match self.protocol {
            WireProtocol::Jtag => jtag_clock(self.pack()?, khz),
            WireProtocol::Swd => SwdClock::new(khz, self.capabilities.swd_5mhz()).map(Clock::Swd),
        }
    }

    /// Stores the request and resolves it against the protocol's clock table.
    pub(crate) fn set_speed(&mut self, khz: u32) -> Result<u32, DebugProbeError> {
        let clock = self.resolve(khz)?;
        self.requested_khz = Some(khz);
        self.clock = Some(clock);
        Ok(clock.khz())
    }

    /// Re-resolves the requested speed for a newly selected protocol; a request the new
    /// protocol cannot honour falls back to its default.
    pub(crate) fn select_protocol(
        &mut self,
        protocol: WireProtocol,
    ) -> Result<(), DebugProbeError> {
        self.protocol = protocol;
        let khz = self.requested_khz.unwrap_or(self.default_khz());
        self.clock = Some(match self.resolve(khz) {
            Ok(clock) => clock,
            Err(DebugProbeError::UnsupportedSpeed(_)) => {
                let default = self.default_khz();
                tracing::warn!("{khz} kHz is not available over {protocol}, using {default} kHz");
                self.resolve(default)?
            }
            Err(e) => return Err(e),
        });
        Ok(())
    }

    pub(crate) fn speed_khz(&self) -> u32 {
        self.clock.map(Clock::khz).unwrap_or(self.default_khz())
    }

    fn clock(&mut self) -> Result<Clock, DebugProbeError> {
        if self.clock.is_none() {
            self.select_protocol(self.protocol)?;
        }
        Ok(self.clock.expect("select_protocol stores the clock"))
    }

    pub(crate) fn swd_clock(&self) -> SwdClock {
        match self.clock {
            Some(Clock::Swd(clock)) => clock,
            _ => SwdClock::fastest(self.capabilities.swd_5mhz()),
        }
    }

    /// Runs the protocol's init with the resolved clock.
    pub(crate) fn attach(&mut self) -> Result<(), DebugProbeError> {
        match self.clock()? {
            Clock::Jtag { index, khz } => {
                if self.jtag_init(index)? != 0 {
                    return Err(DebugProbeError::UnsupportedSpeed(khz));
                }
            }
            Clock::Swd(clock) => {
                let mut payload = [0; 8];
                payload[..4].copy_from_slice(&SwdClock::BASE_HZ.to_le_bytes());
                payload[4] = clock.divisor();
                // The reply byte's meaning is undocumented; the firmware answers 0 on every
                // divisor seen so far.
                let status = self.command_status(CMD_SWD_INIT, &payload)?;
                tracing::debug!(
                    "SWD init divisor {} answered {status:#04x}",
                    clock.divisor()
                );
            }
        }
        Ok(())
    }

    pub(crate) fn detach(&mut self) -> Result<(), DebugProbeError> {
        self.flush_jtag()
    }
}

pub(super) fn list_ch347_devices() -> Vec<ProbeListItem> {
    match nusb::list_devices().wait() {
        Ok(devices) => devices
            .filter(is_ch34x_device)
            .map(|device| {
                let info = DebugProbeInfo::new(
                    device.product_string().unwrap_or("CH347"),
                    device.vendor_id(),
                    device.product_id(),
                    device.serial_number().map(Into::into),
                    &Ch347Factory,
                    None,
                    false,
                );
                ProbeListItem {
                    info,
                    accessibility: usb_probe_accessibility(&device),
                }
            })
            .collect(),
        Err(e) => {
            tracing::warn!("error listing CH347 devices: {e}");
            vec![]
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// The frames a test expects, each with the reply it gets; an empty reply is a timeout.
    #[derive(Debug, Default)]
    pub(crate) struct Script(Mutex<VecDeque<(Vec<u8>, Vec<u8>)>>);

    impl Script {
        pub fn finished(&self) -> bool {
            self.0.lock().unwrap().is_empty()
        }
    }

    #[derive(Debug)]
    pub(crate) struct MockTransport {
        script: Arc<Script>,
        pending: Option<Vec<u8>>,
    }

    impl Transport for MockTransport {
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            let (expected, reply) = self
                .script
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected write");
            assert_eq!(data, &expected[..], "unexpected frame");
            self.pending = Some(reply);
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let reply = self.pending.take().expect("read without a write");
            if reply.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "scripted timeout"));
            }
            // Like the USB layer, a reply longer than the buffer is an error.
            if reply.len() > buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "reply too long"));
            }
            buf[..reply.len()].copy_from_slice(&reply);
            Ok(reply.len())
        }
    }

    pub(crate) const CH347F_1_20: Capabilities = Capabilities {
        variant: Variant::Ch347F,
        firmware: 0x120,
    };

    /// A device whose transport follows the script.
    pub(crate) fn scripted(
        capabilities: Capabilities,
        frames: &[(&[u8], &[u8])],
    ) -> (Ch347Device, Arc<Script>) {
        let script = Arc::new(Script(Mutex::new(
            frames
                .iter()
                .map(|(write, reply)| (write.to_vec(), reply.to_vec()))
                .collect(),
        )));
        let transport = MockTransport {
            script: script.clone(),
            pending: None,
        };
        let mut dev = Ch347Device::new(Box::new(transport), capabilities);
        dev.wait_backoff = Duration::ZERO;
        (dev, script)
    }

    /// A generic CH347.
    pub(crate) fn device(frames: &[(&[u8], &[u8])]) -> (Ch347Device, Arc<Script>) {
        scripted(CH347F_1_20, frames)
    }

    pub(crate) const PACK_PROBE: &[u8] = &[0xD0, 6, 0, 0, 9, 0x22, 0x22, 0x22, 0x22];

    #[test]
    fn the_5mhz_gate_caps_the_swd_default() {
        // A firmware past the gate (see `Capabilities::swd_5mhz`) defaults SWD to 5 MHz.
        let (mut dev, _) = device(&[]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.speed_khz(), 5000);
        // One below it caps at 1 MHz instead.
        let pre_gate = Capabilities {
            variant: Variant::Ch347F,
            firmware: 0x100,
        };
        let (mut dev, _) = scripted(pre_gate, &[]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.speed_khz(), 1000);
    }

    #[test]
    fn speed_is_resolved_per_protocol_and_sent_at_attach() {
        let (mut dev, script) = device(&[
            (PACK_PROBE, &[0xD0, 1, 0, 1]),
            (
                &[0xD0, 6, 0, 0, 3, 0x22, 0x22, 0x22, 0x22],
                &[0xD0, 1, 0, 0],
            ),
        ]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.set_speed(4000).unwrap(), 1000);
        dev.select_protocol(WireProtocol::Jtag).unwrap();
        assert_eq!(dev.speed_khz(), 3750);
        dev.attach().unwrap();
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.speed_khz(), 1000);
        assert!(script.finished());
    }

    #[test]
    fn swd_attach_sends_the_divisor_and_skips_the_pack_probe() {
        let (mut dev, script) = device(&[(
            &[0xE5, 8, 0, 0x40, 0x42, 0x0F, 0x00, 4, 0, 0, 0],
            &[0xE5, 1, 0, 0],
        )]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.set_speed(300).unwrap(), 250);
        dev.attach().unwrap();
        assert!(script.finished());
    }

    #[test]
    fn mismatched_reply_poisons_the_device() {
        let init: &[u8] = &[0xE5, 8, 0, 0x40, 0x42, 0x0F, 0x00, 0, 0, 0, 0];
        let (mut dev, script) = device(&[(init, &[0xD0, 1, 0, 0]), (init, &[0xE5, 1, 0, 0])]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert!(dev.attach().is_err());
        assert!(dev.attach().is_err());
        assert!(!script.finished());
    }

    #[test]
    fn timeout_poisons_the_device() {
        let init: &[u8] = &[0xE5, 8, 0, 0x40, 0x42, 0x0F, 0x00, 0, 0, 0, 0];
        let (mut dev, script) = device(&[(init, &[]), (init, &[0xE5, 1, 0, 0])]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert!(matches!(dev.attach(), Err(DebugProbeError::Usb(_))));
        assert!(dev.attach().is_err());
        assert!(!script.finished());
    }

    #[test]
    fn unsupported_speed_falls_back_to_the_default_on_protocol_change() {
        let (mut dev, script) = device(&[(PACK_PROBE, &[0xD0, 1, 0, 1])]);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert_eq!(dev.set_speed(100).unwrap(), 100);
        dev.select_protocol(WireProtocol::Jtag).unwrap();
        assert_eq!(dev.speed_khz(), 15000);
        assert!(script.finished());
    }
}
