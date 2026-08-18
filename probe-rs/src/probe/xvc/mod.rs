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
            ArmCommunicationInterface, ArmDebugInterface, ArmError,
            communication_interface::DapProbe, sequences::ArmDebugSequence,
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
        AutoImplementJtagAccess, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
        IoSequenceItem, JtagAccess, JtagDriverState, ProbeFactory, RawJtagIo, RawSwdIo,
        SwdSettings, WireProtocol,
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
            jtag_state: JtagDriverState::default(),
            swd_settings: SwdSettings::default(),
        }))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        // XVC servers are reached over the network and cannot be enumerated.
        Vec::new()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        // Surface exactly the requested probe when the selector explicitly
        // targets an XVC endpoint (placeholder VID/PID plus a server address).
        if let Some(selector) = selector
            && selector.vendor_id == XVC_VID
            && selector.product_id == XVC_PID
            && let Some(address) = selector.serial_number.clone()
            && !address.is_empty()
        {
            return vec![DebugProbeInfo {
                identifier: "XVC".to_string(),
                vendor_id: XVC_VID,
                product_id: XVC_PID,
                serial_number: Some(address),
                probe_factory: &Self,
                is_hid_interface: false,
                interface: None,
            }];
        }

        Vec::new()
    }
}

/// An XVC (Xilinx Virtual Cable) debug probe.
#[derive(Debug)]
pub struct XvcProbe {
    device: XvcDevice,
    jtag_state: JtagDriverState,
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
        // The TCP connection is established when the probe is opened, so there
        // is nothing to initialize here.
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

    fn try_as_jtag_probe(&mut self) -> Option<&mut dyn JtagAccess> {
        Some(self)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        Ok(ArmCommunicationInterface::create(self, sequence, true))
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

impl AutoImplementJtagAccess for XvcProbe {}
impl DapProbe for XvcProbe {}

impl RawJtagIo for XvcProbe {
    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);
        self.device.shift_bit(tms, tdi, capture)?;
        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<bitvec::prelude::BitVec, DebugProbeError> {
        self.device.read_captured_bits()
    }

    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
}

impl RawSwdIo for XvcProbe {
    fn swd_io<S>(&mut self, _swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
        // XVC is a JTAG-only transport.
        Err(DebugProbeError::NotImplemented {
            function_name: "swd_io",
        })
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "swj_pins",
        })
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }
}
