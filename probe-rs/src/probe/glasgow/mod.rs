//! Glasgow Interface Explorer probe implementation.
//!
//! This implementation is compatible with the `probe-rs` applet. The Glasgow toolkit must first
//! be used to build the bitstream and configure the device; probe-rs cannot do that itself.

use super::{
    BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbe, DebugProbeError,
    DebugProbeInfo, DebugProbeSelector, ProbeFactory, Results, WireProtocol,
    list::ProbeListItem,
    swd::{Direction, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError},
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

    fn list_probes(&self) -> Vec<ProbeListItem> {
        // Don't return anything; we don't know whether any given device is running a compatible
        // bitstream, and there is no way for us to know which interfaces are bound to the probe-rs
        // applet. These parameters must be specified by the user.
        Vec::new()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
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
            // The probe is built from the selector, so there is no enumerated device to check
            // accessibility against here.
            return vec![ProbeListItem::accessible(DebugProbeInfo {
                identifier: "Glasgow".to_owned(),
                vendor_id: *vendor_id,
                product_id: *product_id,
                serial_number: serial_number.clone(),
                is_hid_interface: false,
                probe_factory: &Self,
                interface: *interface,
            })];
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

    fn swd_batch_cmd(
        &mut self,
        is_ap: bool,
        is_read: bool,
        addr: u8,
        data: Option<u32>,
    ) -> Result<(), DebugProbeError> {
        self.send(
            Target::Swd,
            &[proto::swd::CMD_TRANSFER | (is_ap as u8) | (is_read as u8) << 1 | (addr & 0b1100)],
        );
        if let Some(data) = data {
            self.send(Target::Swd, &data.to_le_bytes()[..]);
        }
        Ok(())
    }

    fn swd_batch_ack(&mut self) -> Result<Option<u32>, DebugProbeError> {
        let response = self.recv(Target::Swd, 1)?[0];
        if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_DATA {
            Ok(Some(u32::from_le_bytes(
                self.recv(Target::Swd, 4)?.try_into().unwrap(),
            )))
        } else if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_NO_DATA {
            if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_OK {
                Ok(None)
            } else if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_WAIT {
                Err(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse))
            } else if response & proto::swd::RSP_ACK_MASK == proto::swd::RSP_ACK_FAULT {
                Err(DebugProbeError::SwdTransfer(
                    SwdTransferError::FaultResponse,
                ))
            } else {
                unreachable!()
            }
        } else if response & proto::swd::RSP_TYPE_MASK == proto::swd::RSP_TYPE_ERROR {
            Err(DebugProbeError::SwdTransfer(SwdTransferError::Protocol))
        } else {
            unreachable!()
        }
    }
}

fn run_swd_sequence(device: &mut GlasgowDevice, bits: &BitSequence) -> Result<(), DebugProbeError> {
    let mut offset = 0;
    while offset < bits.len() {
        let chunk_len = (bits.len() - offset).min(32);
        let mut value = 0u32;
        for i in 0..chunk_len {
            if bits[offset + i] {
                value |= 1 << i;
            }
        }
        device.swd_sequence(chunk_len as u8, value)?;
        offset += chunk_len;
    }
    Ok(())
}

fn run_swd_idle(device: &mut GlasgowDevice, cycles: u32) -> Result<(), DebugProbeError> {
    let mut remaining = cycles as usize;
    while remaining > 0 {
        let chunk_len = remaining.min(32);
        device.swd_sequence(chunk_len as u8, 0)?;
        remaining -= chunk_len;
    }
    Ok(())
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

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_as_swd_probe(self: Box<Self>) -> Result<Box<dyn SwdProbe>, Box<dyn DebugProbe>> {
        Ok(self)
    }
}

fn map_batch_error(
    error: DebugProbeError,
    results: Results,
    fault_operation: usize,
) -> BatchExecutionError<DebugProbeError> {
    match error {
        DebugProbeError::SwdTransfer(transfer_error) => BatchExecutionError {
            error: BatchError::Specific(DebugProbeError::SwdTransfer(transfer_error)),
            results,
            fault_operation,
        },
        error => BatchExecutionError::new_from_debug_probe_at(error, results, fault_operation),
    }
}

impl SwdProbe for Glasgow {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();

        for (fault_operation, (id, op)) in batch.iter().enumerate() {
            let op_result: Result<(), DebugProbeError> = match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
                    let is_ap = *port == Port::Ap;
                    let is_read = *direction == Direction::Read;
                    match self.device.swd_batch_cmd(
                        is_ap,
                        is_read,
                        *addr,
                        if is_read { None } else { Some(*data) },
                    ) {
                        Ok(()) => {}
                        Err(error) => return Err(map_batch_error(error, results, fault_operation)),
                    }
                    let response = match self.device.swd_batch_ack() {
                        Ok(response) => response,
                        Err(error) => return Err(map_batch_error(error, results, fault_operation)),
                    };
                    if is_read {
                        if id.should_capture() {
                            results.push(id, CommandResult::U32(response.expect("expected data")));
                        }
                    } else if response.is_some() {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            DebugProbeError::Other("unexpected data on SWD write".into()),
                            results,
                            fault_operation,
                        ));
                    }
                    Ok(())
                }
                SwdOp::Sequence(bits) => run_swd_sequence(&mut self.device, bits),
                SwdOp::Idle { cycles } => run_swd_idle(&mut self.device, *cycles),
                SwdOp::Pins { out, select, wait } => {
                    if select.0 != 1 << 7 || !wait.is_zero() {
                        Err(DebugProbeError::CommandNotSupportedByProbe {
                            command_name: "swj_pins",
                        })
                    } else if out.nreset() {
                        self.device.clear_reset()
                    } else {
                        self.device.assert_reset()
                    }
                }
            };

            if let Err(error) = op_result {
                return Err(map_batch_error(error, results, fault_operation));
            }
        }

        Ok(results)
    }

    // The Glasgow applet handles FAULT/WAIT states promptly.
    fn handles_wait(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{SwdSettings, swd::SwdPortError, swd::mock::MockSwdProbe};

    #[test]
    fn glasgow_shaped_probe_handles_wait_without_swdport_retry() {
        // A host test cannot open a Glasgow device. MockSwdProbe with handles_wait
        // models the same contract as Glasgow.
        assert!(!SwdProbe::handles_wait(&MockSwdProbe::new()));
        assert!(SwdProbe::handles_wait(&MockSwdProbe::new().handles_wait()));

        let settings = SwdSettings {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 100,
            max_retry_idle_cycles_after_wait: 128,
            idle_cycles_before_write_verify: 0,
            idle_cycles_after_transfer: 0,
        };
        let mut probe = MockSwdProbe::with_settings(SwdSettings {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 100,
            max_retry_idle_cycles_after_wait: 128,
            idle_cycles_before_write_verify: 0,
            idle_cycles_after_transfer: 0,
        })
        .handles_wait();
        probe.push_response(crate::probe::swd::mock::ScriptedResponse::Wait);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, 0b0100);
        let mut port = crate::probe::SwdPort::new(&mut probe, settings);
        let error = port.run(batch).expect_err("WAIT should fail immediately");
        assert_eq!(
            error,
            SwdPortError::Transfer(SwdTransferError::WaitResponse)
        );
        assert_eq!(probe.transfer_ops().len(), 1);
    }
}
