//! ch347 is a usb bus converter that provides UART, I2C and SPI and Jtag/Swd interface
mod protocol;

use std::slice;

use bitvec::vec::BitVec;
use protocol::Ch347UsbJtagDevice;

use crate::{
    architecture::arm::{
        ArmCommunicationInterface, ArmError, RawDapAccess, RegisterAddress,
        communication_interface::DapProbe,
    },
    probe::{DebugProbe, JtagAccess, ProbeFactory, WireProtocol},
};

use super::{AutoImplementJtagAccess, DebugProbeError, JtagDriverState, RawJtagIo};

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
}

impl ProbeFactory for Ch347UsbJtagFactory {
    fn open(
        &self,
        selector: &super::DebugProbeSelector,
    ) -> Result<Box<dyn super::DebugProbe>, super::DebugProbeError> {
        let ch347 = Ch347UsbJtagDevice::new_from_selector(selector)?;

        tracing::info!("Found ch347 device");
        Ok(Box::new(Ch347UsbJtag {
            device: ch347,
            jtag_state: JtagDriverState::default(),
        }))
    }

    fn list_probes(&self) -> Vec<super::DebugProbeInfo> {
        protocol::list_ch347usbjtag_devices()
    }
}

impl DebugProbe for Ch347UsbJtag {
    fn get_name(&self) -> &str {
        "CH347 USB Jtag"
    }

    fn speed_khz(&self) -> u32 {
        self.device.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, super::DebugProbeError> {
        self.device.set_speed(speed_khz)
    }

    fn attach(&mut self) -> Result<(), super::DebugProbeError> {
        self.device.attach()
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), super::DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }
    fn target_reset_assert(&mut self) -> Result<(), super::DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), super::DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_deassert",
        })
    }

    fn select_protocol(
        &mut self,
        protocol: super::WireProtocol,
    ) -> Result<(), super::DebugProbeError> {
        // TODO: Wait to support jtag interface for RawDapAccess
        if protocol != WireProtocol::Swd {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            self.device.select_protocol(protocol)
        }
    }

    fn active_protocol(&self) -> Option<super::WireProtocol> {
        self.device.active_protocol()
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
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
        (Box<dyn DebugProbe>, ArmError),
    > {
        Ok(ArmCommunicationInterface::create(self, sequence, false))
    }
}

impl AutoImplementJtagAccess for Ch347UsbJtag {}
impl RawJtagIo for Ch347UsbJtag {
    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }
    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);
        self.device.shift_bit(tms, tdi, capture)
    }
    fn read_captured_bits(&mut self) -> Result<bitvec::prelude::BitVec, DebugProbeError> {
        self.device.read_captured_bits()
    }
}

impl DapProbe for Ch347UsbJtag {}
impl RawDapAccess for Ch347UsbJtag {
    fn raw_read_register(
        &mut self,
        address: crate::architecture::arm::RegisterAddress,
    ) -> Result<u32, crate::architecture::arm::ArmError> {
        let mut value = 0;
        self.raw_read_block(address, slice::from_mut(&mut value))?;
        Ok(value)
    }

    fn raw_write_register(
        &mut self,
        address: crate::architecture::arm::RegisterAddress,
        value: u32,
    ) -> Result<(), ArmError> {
        self.raw_write_block(address, slice::from_ref(&value))
    }

    fn jtag_sequence(&mut self, cycles: u8, tms: bool, tdi: u64) -> Result<(), DebugProbeError> {
        let mut data = BitVec::with_capacity(cycles as usize);
        for i in 0..cycles {
            data.push((tdi >> i) & 1 == 1);
        }

        self.shift_raw_sequence(super::JtagSequence {
            tms,
            data,
            tdo_capture: false,
        })?;

        Ok(())
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "swj_pins",
        })
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        if self.active_protocol() == Some(WireProtocol::Jtag) {
            self.device.jtag_seq(bit_len, bits, true)
        } else {
            self.device.swd_seq(bit_len, bits)
        }
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn core_status_notification(
        &mut self,
        _state: crate::CoreStatus,
    ) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn raw_read_block(
        &mut self,
        address: crate::architecture::arm::RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        // TODO: wait to add jtag interface
        if address.is_ap() {
            // consume a command
            self.device.read_reg(address).unwrap();
        }

        let mut left = values.len();
        let mut i = 0;
        while left > 0 {
            let wlen = left.min(72);
            let ptr = i * 72;
            self.device
                .batch_read_reg(address, &mut values[ptr..(ptr + wlen)])?;

            left -= wlen;
            i += 1;
        }

        Ok(())
    }

    fn raw_write_block(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        // TODO: wait to add jtag interfce
        let mut left = values.len();
        let mut i = 0;

        while left > 0 {
            let wlen = left.min(56);
            let ptr = i * 56;
            self.device
                .batch_write_reg(address, &values[ptr..ptr + wlen])?;

            left -= wlen;
            i += 1;
        }

        Ok(())
    }
}
