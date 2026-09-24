//! XVC (Xilinx Virtual Cable) JTAG probe support.
//!
//! XVC tunnels JTAG over TCP, allowing probe-rs to drive a TAP exposed by a
//! remote XVC server (for example an FPGA-based bridge or `hw_server`).
//!
//! Because XVC is a network protocol with no USB identity, the probe cannot be
//! auto-discovered. Select it explicitly by passing the placeholder USB id
//! `0000:0000` together with the server address as the "serial number", e.g.
//!
//! ```text
//! probe-rs ... --probe 0000:0000:192.168.1.123:2542
//! ```
//!
//! The port defaults to 2542 when omitted (`--probe 0:0:192.168.1.123`).

mod protocol;

use std::sync::Arc;

use protocol::XvcDevice;

use crate::{
    architecture::{
        arm::{
            ArmCommunicationInterface, ArmDebugInterface, ArmError, sequences::ArmDebugSequence,
        },
        riscv::{
            communication_interface::{RiscvError, RiscvInterfaceBuilder},
            dtm::jtag_dtm::JtagDtmBuilder,
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState, XtensaError,
        },
    },
    probe::{
        BitbangJtag, BitbangSwd, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
        IoSequenceItem, JtagChain, JtagChainAccess, JtagChainState, ProbeFactory, SwdProbe,
        SwdSettings, TapState, WireProtocol, list::ProbeListItem,
    },
};

/// Placeholder USB vendor id used to select an XVC probe.
///
/// XVC has no USB identity; this value only serves to route a
/// [`DebugProbeSelector`] to the XVC driver.
pub(crate) const XVC_VID: u16 = 0x0000;

/// Placeholder USB product id used to select an XVC probe.
pub(crate) const XVC_PID: u16 = 0x0000;

/// A factory for creating [`XvcProbe`] instances.
#[derive(Debug)]
pub struct XvcFactory;

impl std::fmt::Display for XvcFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XVC")
    }
}

impl ProbeFactory for XvcFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let device = XvcDevice::new_from_selector(selector)?;

        Ok(Box::new(XvcProbe {
            device,
            jtag_state: JtagChainState::default(),
            swd_settings: SwdSettings::default(),
        }))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        // XVC servers are reached over the network and cannot be enumerated.
        Vec::new()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        // Surface exactly the requested probe when the selector explicitly
        // targets an XVC endpoint (placeholder VID/PID plus a server address).
        if let Some(selector) = selector
            && selector.vendor_id == XVC_VID
            && selector.product_id == XVC_PID
            && let Some(address) = selector.serial_number.clone()
            && is_xvc_address(&address)
        {
            return vec![ProbeListItem::accessible(DebugProbeInfo {
                identifier: "XVC".to_string(),
                vendor_id: XVC_VID,
                product_id: XVC_PID,
                serial_number: Some(address),
                probe_factory: &Self,
                is_hid_interface: false,
                interface: None,
            })];
        }

        Vec::new()
    }
}

fn is_xvc_address(serial: &str) -> bool {
    // Drop a trailing numeric port, except for bracketed IPv6 literals.
    let host = if serial.contains('[') {
        serial
    } else if let Some((host, port)) = serial.rsplit_once(':') {
        if !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        serial
    };
    !host.is_empty()
        && host.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']' | b':')
        })
}

/// An XVC (Xilinx Virtual Cable) debug probe.
#[derive(Debug)]
pub struct XvcProbe {
    device: XvcDevice,
    jtag_state: JtagChainState,
    swd_settings: SwdSettings,
}

impl DebugProbe for XvcProbe {
    fn get_name(&self) -> &str {
        "XVC"
    }

    fn speed_khz(&self) -> u32 {
        self.device.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        Ok(self.device.set_speed_khz(speed_khz))
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        let mut chain = JtagChain::new(self);
        chain.scan_chain()?;
        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_deassert",
        })
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if protocol != WireProtocol::Jtag {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        // XVC only carries JTAG.
        Some(WireProtocol::Jtag)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        Some(JtagChain::new(self))
    }

    fn try_as_swd_probe_mut(&mut self) -> Option<&mut dyn SwdProbe> {
        Some(self)
    }

    fn try_as_jtag_chain_access_mut(&mut self) -> Option<&mut dyn JtagChainAccess> {
        Some(self)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        let settings = SwdProbe::swd_settings(self.as_ref());
        Ok(ArmCommunicationInterface::create_jtag(
            self, settings, sequence, true,
        ))
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, RiscvError> {
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, XtensaError> {
        Ok(XtensaCommunicationInterface::new(self, state))
    }
}

impl BitbangSwd for XvcProbe {
    fn swd_io<S>(&mut self, _swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
        Err(DebugProbeError::NotImplemented {
            function_name: "swd_io",
        })
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }
}

impl BitbangJtag for XvcProbe {
    fn tap_state(&mut self) -> &mut TapState {
        &mut self.jtag_state.tap_state
    }

    fn shift(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.device.shift_bit(tms, tdi, capture)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn captured(&mut self) -> Result<bitvec::prelude::BitVec, DebugProbeError> {
        self.device.read_captured_bits()
    }
}

impl JtagChainAccess for XvcProbe {
    fn chain_state(&mut self) -> &mut JtagChainState {
        &mut self.jtag_state
    }

    fn chain_state_ref(&self) -> &JtagChainState {
        &self.jtag_state
    }
}
