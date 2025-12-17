//! Glasgow Interface Explorer probe implementation.
//!
//! This implementation is compatible with the `probe-rs` applet. The Glasgow toolkit must first
//! be used to build the bitstream and configure the device; probe-rs cannot do that itself.

use std::sync::Arc;

use crate::architecture::arm::{
    ArmCommunicationInterface, ArmDebugInterface, ArmError, DapError, RawDapAccess,
    RegisterAddress,
    communication_interface::DapProbe,
    dp::{DpRegister, RdBuff},
    sequences::ArmDebugSequence,
};

use super::{
    DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeFactory, WireProtocol,
};

mod mux;
mod net;
mod proto;
mod usb;

use mux::GlasgowDevice;
use proto::Target;

/// A factory for creating [`Glasgow`] probes.
#[derive(Debug)]
pub struct GlasgowFactory;

impl std::fmt::Display for GlasgowFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Glasgow")
    }
}

impl ProbeFactory for GlasgowFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        tracing::debug!("open({selector:?}");
        Glasgow::new_from_device(GlasgowDevice::new_from_selector(selector)?)
            .map(Box::new)
            .map(DebugProbe::into_probe)
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        // Don't return anything; we don't know whether any given device is running a compatible
        // bitstream, and there is no way for us to know which interfaces are bound to the probe-rs
        // applet. These parameters must be specified by the user.
        Vec::new()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<DebugProbeInfo> {
        // Return exactly the specified probe, if it has the option string (which is referred to
        // here as the serial number).
        if let Some(DebugProbeSelector {
            vendor_id,
            product_id,
            serial_number: serial_number @ Some(_),
            interface,
        }) = selector
            && *vendor_id == usb::VID_QIHW
            && *product_id == usb::PID_GLASGOW
        {
            return vec![DebugProbeInfo {
                identifier: "Glasgow".to_owned(),
                vendor_id: *vendor_id,
                product_id: *product_id,
                serial_number: serial_number.clone(),
                is_hid_interface: false,
                probe_factory: &Self,
                interface: *interface,
            }];
        }

        vec![]
    }
}

impl GlasgowDevice {
    fn identify(&mut self) -> Result<(), DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_IDENTIFY]);
        let identifier = self.recv(Target::Root, proto::root::IDENTIFIER.len())?;
        let utf8_identifier = String::from_utf8_lossy(&identifier);
        tracing::debug!("identify(): {utf8_identifier}");
        if identifier == proto::root::IDENTIFIER {
            Ok(())
        } else {
            Err(DebugProbeError::Other(format!(
                "unsupported probe: {utf8_identifier:?}"
            )))?
        }
    }

    fn get_ref_clock(&mut self) -> Result<u32, DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_GET_REF_CLOCK]);
        Ok(u32::from_le_bytes(
            self.recv(Target::Root, 4)?.try_into().unwrap(),
        ))
    }

    fn get_divisor(&mut self) -> Result<u16, DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_GET_DIVISOR]);
        Ok(u16::from_le_bytes(
            self.recv(Target::Root, 2)?.try_into().unwrap(),
        ))
    }

    fn set_divisor(&mut self, divisor: u16) -> Result<(), DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_SET_DIVISOR]);
        self.send(Target::Root, &u16::to_le_bytes(divisor));
        Ok(())
    }

    fn assert_reset(&mut self) -> Result<(), DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_ASSERT_RESET]);
        self.recv(Target::Root, 0)?;
        Ok(())
    }

    fn clear_reset(&mut self) -> Result<(), DebugProbeError> {
        self.send(Target::Root, &[proto::root::CMD_CLEAR_RESET]);
        self.recv(Target::Root, 0)?;
        Ok(())
    }

    fn swd_sequence(&mut self, len: u8, bits: u32) -> Result<(), DebugProbeError> {
        assert!(len > 0 && len <= 32);
        self.send(
            Target::Swd,
            &[proto::swd::CMD_SEQUENCE | (len & proto::swd::SEQ_LEN_MASK)],
        );
        self.send(Target::Swd, &bits.to_le_bytes()[..]);
        self.recv(Target::Swd, 0)?;
        Ok(())
    }

    fn swd_batch_cmd(&mut self, addr: RegisterAddress, data: Option<u32>) -> Result<(), ArmError> {
        self.send(
            Target::Swd,
            &[proto::swd::CMD_TRANSFER
                | (addr.is_ap() as u8)
                | (data.is_none() as u8) << 1
                | (addr.lsb() & 0b1100)],
        );
        if let Some(data) = data {
            self.send(Target::Swd, &data.to_le_bytes()[..]);
        }
        Ok(())
    }

    fn swd_batch_ack(&mut self) -> Result<Option<u32>, ArmError> {
        let response = self.recv(Target::Swd, 1)?[0];
        if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_DATA {
            Ok(Some(u32::from_le_bytes(
                self.recv(Target::Swd, 4)?.try_into().unwrap(),
            )))
        } else if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_NO_DATA {
            if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_OK {
                Ok(None)
            } else if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_WAIT {
                Err(DapError::WaitResponse)?
            } else if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_FAULT {
                Err(DapError::FaultResponse)?
            } else {
                unreachable!()
            }
        } else if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_ERROR {
            Err(DapError::Protocol(WireProtocol::Swd))?
        } else {
            unreachable!()
        }
    }
}

/// A Glasgow Interface Explorer device.
pub struct Glasgow {
    device: GlasgowDevice,
    ref_clock: u32,
    divisor: u16,
}

impl std::fmt::Debug for Glasgow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Glasgow").finish()
    }
}

impl Glasgow {
    fn new_from_device(mut device: GlasgowDevice) -> Result<Self, DebugProbeError> {
        device.identify()?;
        let ref_clock = device.get_ref_clock()?;
        Ok(Glasgow {
            device,
            ref_clock,
            divisor: 0,
        })
    }
}

impl DebugProbe for Glasgow {
    fn get_name(&self) -> &str {
        "Glasgow Interface Explorer"
    }

    fn speed_khz(&self) -> u32 {
        proto::root::divisor_to_frequency(self.ref_clock, self.divisor) / 1000
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        tracing::debug!("set_speed({speed_khz})");
        self.device.set_divisor(proto::root::frequency_to_divisor(
            self.ref_clock,
            speed_khz * 1000,
        ))?;
        self.divisor = self.device.get_divisor()?;
        Ok(self.speed_khz())
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("attach()");
        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        tracing::debug!("detach()");
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("target_reset()");
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("target_reset_assert()");
        self.device.assert_reset()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("target_reset_deassert()");
        self.device.clear_reset()
    }

    fn active_protocol(&self) -> Option<super::WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn select_protocol(&mut self, protocol: super::WireProtocol) -> Result<(), DebugProbeError> {
        tracing::debug!("select_protocol({protocol})");
        match protocol {
            WireProtocol::Swd => Ok(()),
            _ => Err(DebugProbeError::UnsupportedProtocol(protocol)),
        }
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
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        // The Glasgow applet handles FAULT/WAIT states promptly.
        Ok(ArmCommunicationInterface::create(
            self, sequence, /*use_overrun_detect=*/ false,
        ))
    }
}

impl DapProbe for Glasgow {}

impl RawDapAccess for Glasgow {
    fn raw_read_register(&mut self, address: RegisterAddress) -> Result<u32, ArmError> {
        if address.is_ap() {
            let mut value = 0;
            self.raw_read_block(address, std::slice::from_mut(&mut value))?;
            Ok(value)
        } else {
            self.device.swd_batch_cmd(address, None)?;
            let value = self.device.swd_batch_ack()?.expect("expected data");
            tracing::debug!("raw_read_register({address:x?}) -> {value:x}");
            Ok(value)
        }
    }

    fn raw_read_block(
        &mut self,
        address: RegisterAddress,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        assert!(address.is_ap());
        for _ in 0..values.len() {
            self.device.swd_batch_cmd(address, None)?;
        }
        self.device
            .swd_batch_cmd(RegisterAddress::DpRegister(RdBuff::ADDRESS), None)?;
        let _ = self.device.swd_batch_ack()?.expect("expected data");
        for value in values.iter_mut() {
            *value = self.device.swd_batch_ack()?.expect("expected data");
        }
        tracing::debug!(
            "raw_read_block({address:x?}, {}) -> {values:x?}",
            values.len()
        );
        Ok(())
    }

    fn raw_write_register(&mut self, address: RegisterAddress, value: u32) -> Result<(), ArmError> {
        tracing::debug!("raw_write_register({address:x?}, {value:x})");
        self.device.swd_batch_cmd(address, Some(value))?;
        let response = self.device.swd_batch_ack()?;
        assert!(response.is_none(), "unexpected data");
        Ok(())
    }

    fn raw_write_block(
        &mut self,
        address: RegisterAddress,
        values: &[u32],
    ) -> Result<(), ArmError> {
        tracing::debug!("raw_write_block({address:x?}, {values:x?})");
        assert!(address.is_ap());
        for value in values {
            self.device.swd_batch_cmd(address, Some(*value))?;
        }
        for _ in 0..values.len() {
            let response = self.device.swd_batch_ack()?;
            assert!(response.is_none(), "unexpected data");
        }
        Ok(())
    }

    fn jtag_sequence(&mut self, cycles: u8, tms: bool, tdi: u64) -> Result<(), DebugProbeError> {
        tracing::debug!("jtag_sequence({cycles}, {tms}, {tdi})");
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "jtag_sequence",
        })
    }

    fn swj_sequence(&mut self, len: u8, bits: u64) -> Result<(), DebugProbeError> {
        tracing::debug!("swj_sequence({len}, {bits:#x})");
        if len > 0 {
            self.device.swd_sequence(len.min(32), bits as u32)?;
        }
        if len > 32 {
            self.device.swd_sequence(len - 32, (bits >> 32) as u32)?;
        }
        Ok(())
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        tracing::debug!("swj_pins({pin_out:#010b}, {pin_select:#010b}, {pin_wait:#010b})");
        const PIN_NSRST: u32 = 0x80;
        if pin_select != PIN_NSRST || pin_wait != 0 {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins",
            })
        } else {
            if pin_out & PIN_NSRST == 0 {
                self.device.assert_reset()?;
            } else {
                self.device.clear_reset()?;
            }
            Ok(0)
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
}
