//! CMSIS-DAP as a probe that runs DP and AP accesses in its firmware over JTAG.

use std::time::Duration;

use crate::{
    architecture::arm::{
        ArmError, DapError, DapProbe, DebugPortWire, Pins, RegisterAddress,
        communication_interface::{JtagDebugPortWire, dp_wire_register},
        dp::{Abort, Ctrl, DpRegister},
    },
    probe::{
        BatchError, BitSequence, DebugProbe, DebugProbeError, JtagChain, Results, WireProtocol,
        cmsisdap::CmsisDap,
        jtag::dap::jtag_access_registers,
        swd::{Port, SwdBatch, SwdProbe, SwdTransferError},
    },
};

impl CmsisDap {
    fn jtag_wire(&mut self) -> JtagDebugPortWire<'_> {
        let settings = SwdProbe::swd_settings(self);
        JtagDebugPortWire::new(self, settings)
    }

    /// Runs the accesses with `DAP_Transfer`, reporting a failed AP access as the scan chain
    /// does.
    fn run_dap_transfers(
        &mut self,
        accesses: &[(RegisterAddress, Option<u32>)],
        values: &mut std::slice::IterMut<'_, u32>,
    ) -> Result<(), ArmError> {
        if accesses.is_empty() {
            return Ok(());
        }

        let mut batch = SwdBatch::new();
        let mut handles = Vec::new();
        for &(address, value) in accesses {
            let port = if address.is_ap() { Port::Ap } else { Port::Dp };
            match value {
                Some(value) => batch.write(port, address.a2_and_3(), value),
                None => handles.push(batch.read(port, address.a2_and_3())),
            }
        }
        // A failed AP access over JTAG is acknowledged as OK, and only shows in the sticky flags.
        let ctrl = accesses
            .iter()
            .any(|(address, _)| address.is_ap())
            .then(|| batch.read(Port::Dp, RegisterAddress::from(Ctrl::ADDRESS).a2_and_3()));

        let mut results = self.run_jtag_dp_batch(&batch)?;
        for handle in handles {
            *values.next().expect("every read needs a word to land in") = results
                .take(handle)
                .expect("the read was part of the batch");
        }
        if let Some(ctrl) = ctrl {
            let ctrl = Ctrl(results.take(ctrl).expect("the read was part of the batch"));
            if ctrl.sticky_err() {
                self.write_dp_register(Ctrl::ADDRESS, ctrl.into())?;
                return Err(DapError::FaultResponse.into());
            }
        }
        Ok(())
    }

    fn run_jtag_dp_batch(&mut self, batch: &SwdBatch) -> Result<Results, ArmError> {
        let error = match SwdProbe::run_batch(self, batch) {
            Ok(results) => return Ok(results),
            Err(error) => error.error,
        };
        let error = match error {
            BatchError::Specific(DebugProbeError::SwdTransfer(error)) => error,
            BatchError::Probe(error) | BatchError::Specific(error) => {
                return Err(ArmError::Probe(error));
            }
        };
        Err(ArmError::Dap(match error {
            SwdTransferError::WaitResponse => DapError::WaitResponse,
            SwdTransferError::FaultResponse => DapError::FaultResponse,
            SwdTransferError::NoAcknowledge => DapError::NoAcknowledge,
            SwdTransferError::IncorrectParity => DapError::IncorrectParity,
            SwdTransferError::Protocol => DapError::Protocol(WireProtocol::Jtag),
        }))
    }
}

impl DapProbe for CmsisDap {
    fn access_registers(
        &mut self,
        accesses: &[(RegisterAddress, Option<u32>)],
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        if !self.jtag_configured {
            let settings = SwdProbe::swd_settings(self);
            let mut chain = JtagChain::new(self);
            return jtag_access_registers(&mut chain, accesses, values, &settings);
        }
        self.enter_run_test_idle().map_err(ArmError::Probe)?;

        let mut values = values.iter_mut();
        let mut start = 0;
        for (index, &(address, value)) in accesses.iter().enumerate() {
            if let Some(value) = value
                && address == Abort::ADDRESS.into()
            {
                self.run_dap_transfers(&accesses[start..index], &mut values)?;
                self.write_abort(Abort(value))?;
                start = index + 1;
            }
        }
        self.run_dap_transfers(&accesses[start..], &mut values)
    }
}

impl DebugPortWire for CmsisDap {
    fn active_protocol(&self) -> Option<WireProtocol> {
        DebugProbe::active_protocol(self)
    }

    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), ArmError> {
        self.jtag_wire().swj_sequence(bits)
    }

    fn jtag_sequence(&mut self, tms: bool, tdi: &BitSequence) -> Result<(), ArmError> {
        self.jtag_wire().jtag_sequence(tms, tdi)
    }

    fn configure_jtag(&mut self, skip_scan: bool) -> Result<(), ArmError> {
        self.jtag_wire().configure_jtag(skip_scan)
    }

    fn swj_pins(
        &mut self,
        out: Pins,
        select: Pins,
        wait: Duration,
    ) -> Result<Option<Pins>, ArmError> {
        self.jtag_wire().swj_pins(out, select, wait)
    }

    fn target_reset(&mut self) -> Result<(), ArmError> {
        self.jtag_wire().target_reset()
    }

    fn target_reset_assert(&mut self) -> Result<(), ArmError> {
        self.jtag_wire().target_reset_assert()
    }

    fn target_reset_deassert(&mut self) -> Result<(), ArmError> {
        self.jtag_wire().target_reset_deassert()
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn raw_read_register(&mut self, port: Port, addr: u8) -> Result<u32, ArmError> {
        let mut value = [0];
        self.access_registers(&[(dp_wire_register(port, addr), None)], &mut value)?;
        Ok(value[0])
    }

    fn raw_write_register(&mut self, port: Port, addr: u8, value: u32) -> Result<(), ArmError> {
        self.access_registers(&[(dp_wire_register(port, addr), Some(value))], &mut [])
    }

    fn try_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        Some(JtagChain::new(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{
        cmsisdap::commands::{
            CmsisDapDevice,
            fake::{FakeProbe, Reply, standard_reply},
            transfer::Ack,
        },
        jtag::TapState,
    };
    use std::sync::{Arc, Mutex};

    /// A configured probe in JTAG mode whose CTRL/STAT reads return `ctrl`, and all other reads
    /// zero.
    fn jtag_dap(ctrl: u32) -> (CmsisDap, Arc<Mutex<FakeProbe>>) {
        let probe = FakeProbe::shared(64, move |command| match command {
            [0x05, _, count, requests @ ..] => {
                let mut reply = vec![0x05, *count, Ack::Ok as u8];
                let mut requests = requests.iter();
                while let Some(&request) = requests.next() {
                    match request & 0b1111 {
                        0b0110 => reply.extend(ctrl.to_le_bytes()),
                        r if r & 0b10 != 0 => reply.extend([0; 4]),
                        _ => _ = requests.nth(3),
                    }
                }
                Reply::Now(reply)
            }
            [0x08, ..] => Reply::Now(vec![0x08, 0x00]),
            _ => standard_reply(command, 64, 4),
        });
        let device = CmsisDapDevice::Fake(probe.clone());
        let mut dap = CmsisDap::new_from_device(device).unwrap();
        dap.protocol = Some(WireProtocol::Jtag);
        dap.jtag_configured = true;
        dap.jtag_state.tap_state = TapState::RunTestIdle;
        (dap, probe)
    }

    fn sent(probe: &Arc<Mutex<FakeProbe>>, id: u8) -> Vec<Vec<u8>> {
        let sent = &probe.lock().unwrap().sent;
        sent.iter().filter(|c| c[0] == id).cloned().collect()
    }

    #[test]
    fn a_sticky_error_fails_the_accesses_and_is_written_back() {
        let (mut dap, probe) = jtag_dap(0x20);

        let error = dap
            .access_registers(&[(RegisterAddress::ApRegister(0xC), None)], &mut [0])
            .unwrap_err();
        assert!(matches!(error, ArmError::Dap(DapError::FaultResponse)));
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[0x05, 0, 1, 0b0100, 0x20, 0, 0, 0]
        );
    }

    #[test]
    fn an_abort_sends_only_dapabort() {
        let (mut dap, probe) = jtag_dap(0);

        dap.access_registers(&[(Abort::ADDRESS.into(), Some(0x1f))], &mut [])
            .unwrap();
        assert_eq!(sent(&probe, 0x08), [[0x08, 0, 1, 0, 0, 0]]);
        assert!(sent(&probe, 0x05).iter().all(|c| c[3] & 0b10 != 0));
    }

    #[test]
    fn reads_land_in_order_around_an_abort() {
        let (mut dap, probe) = jtag_dap(0);
        let mut values = [1, 1];

        dap.access_registers(
            &[
                (RegisterAddress::ApRegister(0xC), None),
                (Abort::ADDRESS.into(), Some(1)),
                (Ctrl::ADDRESS.into(), None),
            ],
            &mut values,
        )
        .unwrap();
        assert_eq!(values, [0, 0]);
        assert_eq!(sent(&probe, 0x05).len(), 2);
    }
}
