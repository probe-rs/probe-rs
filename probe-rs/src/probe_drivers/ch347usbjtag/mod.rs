//! ch347 is a usb bus converter that provides UART, I2C and SPI and Jtag/Swd interface
mod protocol;

use protocol::Ch347UsbJtagDevice;

use crate::{
    architecture::{
        arm::{ArmCommunicationInterface, communication_interface::DapProbe},
        riscv::dtm::jtag_dtm::JtagDtmBuilder,
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    probe::{DebugProbe, ProbeFactory, WireProtocol},
};

use crate::probe::{
    AutoImplementJtagAccess, DebugProbeError, DebugProbeInfo, DebugProbeSelector, IoSequenceItem,
    JtagAccess, JtagDriverState, RawJtagIo, RawSwdIo, SwdSettings,
};

/// A factory for creating [`Ch347UsbJtag`] instances.
#[derive(Debug)]
pub struct Ch347UsbJtagFactory;

impl std::fmt::Display for Ch347UsbJtagFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ch347UsbJtag")
    }
}

/// An Ch347-based debug probe.
#[derive(Debug)]
pub struct Ch347UsbJtag {
    device: Ch347UsbJtagDevice,
    jtag_state: JtagDriverState,
    swd_settings: SwdSettings,
}

impl ProbeFactory for Ch347UsbJtagFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        let ch347 = Ch347UsbJtagDevice::new_from_selector(selector)?;

        tracing::info!("Found ch347 device");
        Ok(Box::new(Ch347UsbJtag {
            device: ch347,
            jtag_state: JtagDriverState::default(),
            swd_settings: SwdSettings::default(),
        }))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        protocol::list_ch347usbjtag_devices()
    }
}

impl RawJtagIo for Ch347UsbJtag {
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

impl RawSwdIo for Ch347UsbJtag {
    fn swd_io<S>(&mut self, _swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
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

impl AutoImplementJtagAccess for Ch347UsbJtag {}
impl DapProbe for Ch347UsbJtag {}

impl DebugProbe for Ch347UsbJtag {
    fn get_name(&self) -> &str {
        "CH347 USB Jtag"
    }

    fn speed_khz(&self) -> u32 {
        self.device.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        Ok(self.device.set_speed_khz(speed_khz))
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.device.attach()
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        // TODO
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }
    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        // TODO
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        // TODO
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_deassert",
        })
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        // ch347 is support swd, wait...
        // TODO
        if protocol != WireProtocol::Jtag {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        // TODO
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
        sequence: std::sync::Arc<dyn crate::architecture::arm::sequences::ArmDebugSequence>,
    ) -> Result<
        Box<dyn crate::architecture::arm::ArmDebugInterface + 'probe>,
        (Box<dyn DebugProbe>, crate::architecture::arm::ArmError),
    > {
        Ok(ArmCommunicationInterface::create(self, sequence, true))
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<
        Box<
            dyn crate::architecture::riscv::communication_interface::RiscvInterfaceBuilder<'probe>
                + 'probe,
        >,
        crate::architecture::riscv::communication_interface::RiscvError,
    > {
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut crate::architecture::xtensa::communication_interface::XtensaDebugInterfaceState,
    ) -> Result<
        crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface<'probe>,
        crate::architecture::xtensa::communication_interface::XtensaError,
    > {
        Ok(XtensaCommunicationInterface::new(self, state))
    }
}
