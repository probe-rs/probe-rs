//! CMSIS-DAP as an ARM debug interface. The probe firmware runs the DP and AP accesses.

use std::time::Duration;

use crate::{
    architecture::arm::{
        ArmError, DapError, Pins, RegisterAddress,
        communication_interface::{
            ApBatch, DapTransport, probe_debug_port_wire, transfer_error_to_dap,
        },
        dp::{Abort, Ctrl, DpRegister, SelectV1},
        traits::DebugPortWire,
    },
    probe::{
        BatchError, BitSequence, DebugProbe, DebugProbeError, JtagChain, WireProtocol,
        cmsisdap::{
            CmsisDap,
            commands::{
                self,
                transfer::{Ack, TransferRequest},
            },
        },
        jtag::{JtagBatch, JtagProbe, TapState},
        swd::{Port, SwdBatch, SwdProbe, SwdTransferError},
    },
};

/// One DP or AP register access: the port, address bits 2 and 3, and the value of a write.
type Access = (Port, u8, Option<u32>);

impl CmsisDap {
    /// Run the accesses in order with `DAP_Transfer` and `DAP_TransferBlock`.
    ///
    /// Each read writes one word to `values`, in the order of the reads.
    fn transfer(&mut self, accesses: &[Access], values: &mut [u32]) -> Result<(), ArmError> {
        let jtag = self.protocol == Some(WireProtocol::Jtag);
        if jtag && !self.jtag_configured {
            self.track_select(accesses);
            // The probe does not know the scan chain yet, so the accesses run as JTAG scans.
            let mut values = values.iter_mut();
            return probe_debug_port_wire(self, |wire| {
                for &(port, addr, value) in accesses {
                    match value {
                        Some(value) => wire.raw_write_register(port, addr, value)?,
                        None => *values.next().unwrap() = wire.raw_read_register(port, addr)?,
                    }
                }
                Ok(())
            });
        }
        if jtag {
            self.enter_run_test_idle().map_err(ArmError::Probe)?;
        }

        // Over JTAG, ABORT has its own instruction, which DAP_Transfer cannot reach.
        let abort_addr = RegisterAddress::from(Abort::ADDRESS).a2_and_3();
        let is_abort = |&(port, addr, value): &Access| {
            jtag && port == Port::Dp && addr == abort_addr && value.is_some()
        };

        let mut values = values.iter_mut();
        for run in accesses.split_inclusive(is_abort) {
            let (run, abort) = match run.split_last() {
                Some((last, rest)) if is_abort(last) => (rest, last.2),
                _ => (run, None),
            };
            self.transfer_run(run, &mut values, jtag)?;
            if let Some(abort) = abort {
                self.write_abort(Abort(abort))?;
            }
        }
        Ok(())
    }

    fn transfer_run(
        &mut self,
        accesses: &[Access],
        values: &mut std::slice::IterMut<'_, u32>,
        jtag: bool,
    ) -> Result<(), ArmError> {
        if accesses.is_empty() {
            return Ok(());
        }
        // The CTRL/STAT check comes after the accesses, so it sees the last SELECT of the run.
        self.track_select(accesses);

        let mut batch = SwdBatch::new();
        let mut reads = Vec::new();
        for &(port, addr, value) in accesses {
            match value {
                Some(value) => batch.write(port, addr, value),
                None => reads.push(batch.read(port, addr)),
            }
        }
        // Over JTAG, a failed AP access is acknowledged as OK, and only CTRL/STAT shows it.
        let check_ctrl = jtag && accesses.iter().any(|(port, ..)| *port == Port::Ap);
        // In DP bank 0, the check goes in the same batch. Else it needs the bank switch.
        let ctrl_in_batch = check_ctrl && self.select_bank_0() == self.dp_select;
        let ctrl_read = ctrl_in_batch
            .then(|| batch.read(Port::Dp, RegisterAddress::from(Ctrl::ADDRESS).a2_and_3()));

        let mut results = self.run_transfers(&batch)?;
        for read in reads {
            *values.next().unwrap() = results.take(read).unwrap();
        }

        let ctrl = match ctrl_read {
            Some(read) => Some(Ctrl(results.take(read).unwrap())),
            None if check_ctrl => Some(Ctrl(self.access_ctrl(None)?)),
            None => None,
        };
        if let Some(ctrl) = ctrl
            && ctrl.sticky_err()
        {
            tracing::debug!("JTAG transfers failed, CTRL/STAT: {ctrl:?}");
            self.access_ctrl(Some(ctrl.into()))?;
            return Err(DapError::FaultResponse.into());
        }
        Ok(())
    }

    /// Clears the sticky error flags of a JTAG-DP.
    ///
    /// Its ABORT register has no STKERRCLR. The flags clear when they are written back as one.
    pub(super) fn clear_jtag_sticky_err(&mut self) -> Result<(), ArmError> {
        let ctrl = Ctrl(self.access_ctrl(None)?);
        if ctrl.sticky_err() {
            self.access_ctrl(Some(ctrl.into()))?;
        }
        Ok(())
    }

    /// The value of DP SELECT with DP bank 0, where DP address 0x4 is CTRL/STAT.
    fn select_bank_0(&self) -> u32 {
        // DPBANKSEL is in the same bits in the SELECT of every DP version.
        let mut select = SelectV1(self.dp_select);
        select.set_dp_bank_sel(0);
        select.0
    }

    /// Read CTRL/STAT, or write `value` to it, in DP bank 0. Returns the value read.
    ///
    /// This is one `DAP_Transfer` with no error recovery. The recovery of
    /// [`SwdProbe::run_batch`] clears the sticky flags, which over JTAG comes back here.
    ///
    /// A debug sequence can select another bank through the wire. The bank is set back after
    /// the access, also when the access fails.
    fn access_ctrl(&mut self, value: Option<u32>) -> Result<u32, ArmError> {
        let bank_0 = self.select_bank_0();
        let switch = bank_0 != self.dp_select;

        let mut request = TransferRequest::empty();
        request.dap_index = self.jtag_state.chain_params.index as u8;
        if switch {
            request.add_write(SelectV1::ADDRESS.into(), bank_0);
        }
        match value {
            Some(value) => request.add_write(Ctrl::ADDRESS.into(), value),
            None => request.add_read(Ctrl::ADDRESS.into()),
        }
        if switch {
            request.add_write(SelectV1::ADDRESS.into(), self.dp_select);
        }
        let count = 1 + 2 * usize::from(switch);
        let result = commands::send_command(&mut self.device, &request)
            .map_err(|error| ArmError::Probe(DebugProbeError::from(error)))
            .and_then(|response| {
                let last = &response.last_transfer_response;
                if last.protocol_error {
                    let protocol = self.protocol.unwrap_or(WireProtocol::Jtag);
                    return Err(DapError::Protocol(protocol).into());
                }
                match last.ack {
                    Ack::Ok if response.transfers.len() == count => {
                        Ok(response.transfers[usize::from(switch)].data.unwrap_or(0))
                    }
                    Ack::Ok => Err(ArmError::Probe(DebugProbeError::Other(
                        "the probe ran only a part of the transfers".to_string(),
                    ))),
                    Ack::Wait => Err(DapError::WaitResponse.into()),
                    Ack::Fault => Err(DapError::FaultResponse.into()),
                    Ack::NoAck => Err(DapError::NoAcknowledge.into()),
                }
            });

        if switch && result.is_err() {
            // The probe stops at the first failed transfer, so the bank is maybe not set back.
            let mut restore = TransferRequest::write(SelectV1::ADDRESS, self.dp_select);
            restore.dap_index = self.jtag_state.chain_params.index as u8;
            if let Err(error) = commands::send_command(&mut self.device, &restore) {
                tracing::debug!("Failed to select the DP bank again: {error}");
            }
        }
        result
    }

    /// Record the last write to DP SELECT in `accesses`.
    fn track_select(&mut self, accesses: &[Access]) {
        let select_addr = RegisterAddress::from(SelectV1::ADDRESS).a2_and_3();
        if let Some(&(_, _, Some(select))) = accesses.iter().rev().find(|&&(port, addr, value)| {
            port == Port::Dp && addr == select_addr && value.is_some()
        }) {
            self.dp_select = select;
        }
    }

    fn run_transfers(&mut self, batch: &SwdBatch) -> Result<crate::probe::Results, ArmError> {
        let error = match SwdProbe::run_batch(self, batch) {
            Ok(results) => return Ok(results),
            Err(error) => error.error,
        };
        let error = match error {
            BatchError::Probe(DebugProbeError::SwdTransfer(error))
            | BatchError::Specific(DebugProbeError::SwdTransfer(error)) => error,
            BatchError::Probe(error) | BatchError::Specific(error) => {
                return Err(ArmError::Probe(error));
            }
        };
        let protocol = self.protocol.unwrap_or(WireProtocol::Swd);
        if protocol == WireProtocol::Swd && error == SwdTransferError::FaultResponse {
            // The probe clears only STICKYERR. A FAULT can also come from the other flags.
            let mut abort = Abort(0);
            abort.set_orunerrclr(true);
            abort.set_wderrclr(true);
            abort.set_stkerrclr(true);
            abort.set_stkcmpclr(true);
            self.write_abort(abort)?;
        }
        Err(ArmError::Dap(transfer_error_to_dap(error, protocol)))
    }

    /// Put the TAP in Run-Test/Idle, where `DAP_Transfer` over JTAG starts and ends.
    fn enter_run_test_idle(&mut self) -> Result<(), DebugProbeError> {
        if self.jtag_state.tap_state == TapState::RunTestIdle {
            return Ok(());
        }
        let mut batch = JtagBatch::new();
        batch.enter(TapState::RunTestIdle);
        JtagProbe::run_batch(self, &batch)
            .map(drop)
            .map_err(|error| match error.error {
                BatchError::Probe(error) | BatchError::Specific(error) => error,
            })
    }
}

/// The wire for the debug sequences. Register accesses run in the probe firmware.
struct CmsisDapWire<'a>(&'a mut CmsisDap);

impl DebugPortWire for CmsisDapWire<'_> {
    fn active_protocol(&self) -> Option<WireProtocol> {
        self.0.protocol
    }

    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), ArmError> {
        probe_debug_port_wire(self.0, |wire| wire.swj_sequence(bits))
    }

    fn jtag_sequence(&mut self, tms: bool, tdi: &BitSequence) -> Result<(), ArmError> {
        probe_debug_port_wire(self.0, |wire| wire.jtag_sequence(tms, tdi))
    }

    fn configure_jtag(&mut self, skip_scan: bool) -> Result<(), ArmError> {
        probe_debug_port_wire(self.0, |wire| wire.configure_jtag(skip_scan))
    }

    fn swj_pins(
        &mut self,
        out: Pins,
        select: Pins,
        wait: Duration,
    ) -> Result<Option<Pins>, ArmError> {
        let levels = probe_debug_port_wire(self.0, |wire| wire.swj_pins(out, select, wait))?;
        if select.ntrst() && !out.ntrst() {
            // nTRST puts the TAP in Test-Logic-Reset.
            self.0.jtag_state.tap_state = TapState::TestLogicReset;
        }
        Ok(levels)
    }

    fn target_reset(&mut self) -> Result<(), ArmError> {
        DebugProbe::target_reset(self.0).map_err(ArmError::Probe)
    }

    fn target_reset_assert(&mut self) -> Result<(), ArmError> {
        DebugProbe::target_reset_assert(self.0).map_err(ArmError::Probe)
    }

    fn target_reset_deassert(&mut self) -> Result<(), ArmError> {
        DebugProbe::target_reset_deassert(self.0).map_err(ArmError::Probe)
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        Ok(())
    }

    fn raw_read_register(&mut self, port: Port, addr: u8) -> Result<u32, ArmError> {
        let mut value = [0];
        self.0.transfer(&[(port, addr, None)], &mut value)?;
        Ok(value[0])
    }

    fn raw_write_register(&mut self, port: Port, addr: u8, value: u32) -> Result<(), ArmError> {
        self.0.transfer(&[(port, addr, Some(value))], &mut [])
    }

    fn try_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        // The caller can change the scan chain. Until it calls `configure_jtag` again, the
        // accesses run as JTAG scans.
        self.0.jtag_configured = false;
        Some(JtagChain::new(self.0))
    }
}

/// Runs the accesses of an ARM debug interface in the CMSIS-DAP probe firmware.
#[derive(Debug)]
pub(crate) struct CmsisDapTransport(pub(crate) Box<CmsisDap>);

impl CmsisDapTransport {
    fn transfer_register(
        &mut self,
        register: RegisterAddress,
        value: Option<u32>,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let port = if register.is_ap() { Port::Ap } else { Port::Dp };
        self.0
            .transfer(&[(port, register.a2_and_3(), value)], values)
    }
}

impl DapTransport for CmsisDapTransport {
    fn probe(&mut self) -> &mut dyn DebugProbe {
        self.0.as_mut()
    }

    fn probe_ref(&self) -> &dyn DebugProbe {
        self.0.as_ref()
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self.0
    }

    fn protocol(&self) -> Option<WireProtocol> {
        self.0.protocol
    }

    fn wire(&mut self) -> Box<dyn DebugPortWire + '_> {
        Box::new(CmsisDapWire(&mut self.0))
    }

    fn read(&mut self, register: RegisterAddress) -> Result<u32, ArmError> {
        let mut value = [0];
        self.transfer_register(register, None, &mut value)?;
        Ok(value[0])
    }

    fn write(&mut self, register: RegisterAddress, value: u32) -> Result<(), ArmError> {
        self.transfer_register(register, Some(value), &mut [])
    }

    fn read_ap_repeated(&mut self, address: u8, values: &mut [u32]) -> Result<(), ArmError> {
        let addr = RegisterAddress::ApRegister(address).a2_and_3();
        let accesses = vec![(Port::Ap, addr, None); values.len()];
        self.0.transfer(&accesses, values)
    }

    fn write_ap_repeated(&mut self, address: u8, values: &[u32]) -> Result<(), ArmError> {
        let addr = RegisterAddress::ApRegister(address).a2_and_3();
        let accesses: Vec<Access> = values
            .iter()
            .map(|&value| (Port::Ap, addr, Some(value)))
            .collect();
        self.0.transfer(&accesses, &mut [])
    }

    fn ap_batch(&mut self) -> Option<&mut dyn ApBatch> {
        Some(self)
    }
}

impl ApBatch for CmsisDapTransport {
    fn access_ap(
        &mut self,
        accesses: &[(u64, Option<u32>)],
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let accesses: Vec<Access> = accesses
            .iter()
            .map(|&(address, value)| {
                let addr = RegisterAddress::ApRegister(address as u8).a2_and_3();
                (Port::Ap, addr, value)
            })
            .collect();
        self.0.transfer(&accesses, values)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::architecture::arm::{
        ApV2Address, ArmDebugInterface, FullyQualifiedApAddress, dp::DpAddress,
        sequences::ArmDebugSequence,
    };
    use crate::probe::cmsisdap::commands::{
        CmsisDapDevice,
        fake::{FakeProbe, Reply, standard_reply},
        transfer::Ack,
    };

    /// A configured probe in JTAG mode. CTRL/STAT reads return `ctrl`, other reads return zero.
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

    /// The commands with `id` that the probe got.
    fn sent(probe: &Arc<Mutex<FakeProbe>>, id: u8) -> Vec<Vec<u8>> {
        let sent = &probe.lock().unwrap().sent;
        sent.iter().filter(|c| c[0] == id).cloned().collect()
    }

    #[test]
    fn a_sticky_error_fails_the_accesses_and_is_written_back() {
        let (mut dap, probe) = jtag_dap(0x20);

        let error = dap
            .transfer(&[(Port::Ap, 0xC, None)], &mut [0])
            .unwrap_err();
        assert!(matches!(error, ArmError::Dap(DapError::FaultResponse)));
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[0x05, 0, 1, 0b0100, 0x20, 0, 0, 0]
        );
    }

    #[test]
    fn ctrl_stat_is_read_in_dp_bank_0() {
        // A debug sequence selects DP bank 1 through the wire, then accesses an AP.
        let (mut dap, probe) = jtag_dap(0);

        dap.transfer(&[(Port::Dp, 0x8, Some(0x0100_0001))], &mut [])
            .unwrap();
        dap.transfer(&[(Port::Ap, 0xC, None)], &mut [0]).unwrap();
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[
                0x05, 0, 3, 0b1000, 0, 0, 0, 0x01, 0b0110, 0b1000, 0x01, 0, 0, 0x01
            ]
        );
    }

    #[test]
    fn a_failed_ctrl_stat_access_returns_and_selects_the_dp_bank_again() {
        // CTRL/STAT reads answer FAULT. Other transfers succeed.
        let probe = FakeProbe::shared(64, |command| match command {
            [0x05, _, count, requests @ ..] => {
                let mut reply = vec![0x05, *count, Ack::Ok as u8];
                let mut requests = requests.iter();
                while let Some(&request) = requests.next() {
                    match request & 0b1111 {
                        0b0110 => return Reply::Now(vec![0x05, 0, Ack::Fault as u8]),
                        r if r & 0b10 != 0 => reply.extend([0; 4]),
                        _ => _ = requests.nth(3),
                    }
                }
                Reply::Now(reply)
            }
            _ => standard_reply(command, 64, 4),
        });
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        dap.protocol = Some(WireProtocol::Jtag);
        dap.jtag_configured = true;
        dap.jtag_state.tap_state = TapState::RunTestIdle;
        dap.transfer(&[(Port::Dp, 0x8, Some(0x0100_0001))], &mut [])
            .unwrap();

        // Clearing the sticky flags reads CTRL/STAT. Its FAULT must not start another clear.
        let error = dap.write_abort(Abort(0x1e)).unwrap_err();
        assert!(matches!(error, ArmError::Dap(DapError::FaultResponse)));
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[0x05, 0, 1, 0b1000, 0x01, 0, 0, 0x01]
        );
    }

    #[test]
    fn an_abort_sends_only_dapabort() {
        let (mut dap, probe) = jtag_dap(0);

        dap.transfer(&[(Port::Dp, 0, Some(0x1f))], &mut []).unwrap();
        assert_eq!(sent(&probe, 0x08), [[0x08, 0, 1, 0, 0, 0]]);
        // The clear bits read CTRL/STAT, and write nothing when no flag is set.
        assert!(sent(&probe, 0x05).iter().all(|c| c[3] & 0b10 != 0));
    }

    #[test]
    fn an_abort_without_dapabort_clears_the_sticky_flags_through_ctrl_stat() {
        let (mut dap, probe) = jtag_dap(0x20);

        dap.transfer(&[(Port::Dp, 0, Some(0x1e))], &mut []).unwrap();
        assert!(sent(&probe, 0x08).is_empty());
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[0x05, 0, 1, 0b0100, 0x20, 0, 0, 0]
        );
    }

    #[test]
    fn reads_land_in_order_around_an_abort() {
        let (mut dap, probe) = jtag_dap(0);
        let mut values = [1, 1];

        dap.transfer(
            &[
                (Port::Ap, 0xC, None),
                (Port::Dp, 0, Some(1)),
                (Port::Dp, 0x4, None),
            ],
            &mut values,
        )
        .unwrap();
        assert_eq!(values, [0, 0]);
        assert_eq!(sent(&probe, 0x05).len(), 2);
    }

    #[test]
    fn a_swd_fault_clears_all_sticky_flags() {
        // AP accesses answer FAULT. CTRL/STAT shows STICKYERR, other reads return zero.
        let probe = FakeProbe::shared(64, |command| match command {
            [0x05, _, _, request, ..] if request & 1 != 0 => {
                Reply::Now(vec![0x05, 0, Ack::Fault as u8])
            }
            [0x05, _, count, requests @ ..] => {
                let mut reply = vec![0x05, *count, Ack::Ok as u8];
                let mut requests = requests.iter();
                while let Some(&request) = requests.next() {
                    match request & 0b1111 {
                        0b0110 => reply.extend(0x20u32.to_le_bytes()),
                        r if r & 0b10 != 0 => reply.extend([0; 4]),
                        _ => _ = requests.nth(3),
                    }
                }
                Reply::Now(reply)
            }
            _ => standard_reply(command, 64, 4),
        });
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        dap.protocol = Some(WireProtocol::Swd);

        let error = dap
            .transfer(&[(Port::Ap, 0xC, None)], &mut [0])
            .unwrap_err();
        assert!(matches!(error, ArmError::Dap(DapError::FaultResponse)));
        assert_eq!(
            sent(&probe, 0x05).last().unwrap(),
            &[0x05, 0, 1, 0b0000, 0x1E, 0, 0, 0]
        );
    }

    #[test]
    fn a_changed_chain_runs_the_accesses_as_scans() {
        let (mut dap, _probe) = jtag_dap(0);

        let _ = CmsisDapWire(&mut dap).try_jtag_chain();
        assert!(!dap.jtag_configured);
    }

    /// The DPIDR of a DPv2.
    const DPV2: u32 = 0x2BA0_1477;

    /// The DPIDR of a DPv3.
    const DPV3: u32 = 0x2BA0_3477;

    /// Answers each `DAP_Transfer` and `DAP_TransferBlock` request as a powered-up DP with
    /// `dpidr`, and connects over `port` (1 is SWD, 2 is JTAG). `answer` can replace a reply.
    fn firmware_dap(
        port: u8,
        dpidr: u32,
        mut answer: impl FnMut(&[u8]) -> Option<Reply> + Send + 'static,
    ) -> (Box<CmsisDap>, Arc<Mutex<FakeProbe>>) {
        let probe = FakeProbe::shared(64, move |command| {
            if let Some(reply) = answer(command) {
                return reply;
            }
            match command {
                // SWD and JTAG.
                [0x00, 0xF0, ..] => Reply::Now(vec![0x00, 0x01, 0x03]),
                [0x02, ..] => Reply::Now(vec![0x02, port]),
                [0x05, _, count, requests @ ..] => {
                    let mut reply = vec![0x05, *count, Ack::Ok as u8];
                    let mut requests = requests.iter();
                    while let Some(&request) = requests.next() {
                        match request & 0b1111 {
                            0b0010 => reply.extend(dpidr.to_le_bytes()),
                            // CTRL/STAT, with the power-up requests acknowledged.
                            0b0110 => reply.extend(0xF000_0000u32.to_le_bytes()),
                            r if r & 0b10 != 0 => reply.extend([0; 4]),
                            _ => _ = requests.nth(3),
                        }
                    }
                    Reply::Now(reply)
                }
                [0x06, _, lo, hi, request, ..] => {
                    let mut reply = vec![0x06, *lo, *hi, Ack::Ok as u8];
                    if request & 0b10 != 0 {
                        reply.resize(
                            reply.len() + 4 * usize::from(u16::from_le_bytes([*lo, *hi])),
                            0,
                        );
                    }
                    Reply::Now(reply)
                }
                [0x10, ..] => Reply::Now(vec![0x10, 0xFF]),
                [0x14, count, sequences @ ..] => {
                    let mut reply = vec![0x14, 0x00];
                    let mut sequences = sequences.iter();
                    for _ in 0..*count {
                        let info = *sequences.next().unwrap();
                        let clocks = match info & 0x3F {
                            0 => 64,
                            clocks => usize::from(clocks),
                        };
                        let bytes = clocks.div_ceil(8);
                        _ = sequences.nth(bytes - 1);
                        if info & 0x80 != 0 {
                            reply.resize(reply.len() + bytes, 0);
                        }
                    }
                    Reply::Now(reply)
                }
                [id @ (0x04 | 0x08 | 0x11 | 0x12 | 0x13 | 0x15), ..] => Reply::Now(vec![*id, 0x00]),
                _ => standard_reply(command, 64, 4),
            }
        });
        let device = CmsisDapDevice::Fake(probe.clone());
        let dap = CmsisDap::new_from_device(device).unwrap();
        (Box::new(dap), probe)
    }

    /// Tells the probe the scan chain without a scan, as the TI sequences do.
    #[derive(Debug)]
    struct KnownChain;

    impl ArmDebugSequence for KnownChain {
        fn debug_port_setup(
            &self,
            interface: &mut dyn DebugPortWire,
            _dp: DpAddress,
        ) -> Result<(), ArmError> {
            interface.configure_jtag(true)
        }
    }

    /// Attaches to a DP with `dpidr` over `protocol`. The probe keeps no commands from the attach.
    fn attached(
        protocol: WireProtocol,
        dpidr: u32,
        answer: impl FnMut(&[u8]) -> Option<Reply> + Send + 'static,
    ) -> (Box<dyn ArmDebugInterface>, Arc<Mutex<FakeProbe>>) {
        let port = match protocol {
            WireProtocol::Swd => 1,
            WireProtocol::Jtag => 2,
        };
        let (mut dap, probe) = firmware_dap(port, dpidr, answer);
        let sequence: Arc<dyn ArmDebugSequence> = match protocol {
            WireProtocol::Swd => crate::architecture::arm::sequences::DefaultArmSequence::create(),
            WireProtocol::Jtag => Arc::new(KnownChain),
        };
        dap.jtag_state.expected_scan_chain = Some(vec![probe_rs_target::ScanChainElement {
            name: None,
            ir_len: Some(4),
        }]);
        DebugProbe::select_protocol(dap.as_mut(), protocol).unwrap();
        DebugProbe::attach(dap.as_mut()).unwrap();
        let mut interface = dap
            .try_get_arm_debug_interface(sequence)
            .map_err(|(_, error)| error)
            .unwrap();
        interface.select_debug_port(DpAddress::Default).unwrap();
        probe.lock().unwrap().sent.clear();
        (interface, probe)
    }

    /// The requests in the `DAP_Transfer` commands that the probe got: the request bits and the
    /// value of a write.
    fn transfer_requests(probe: &Arc<Mutex<FakeProbe>>) -> Vec<(u8, Option<u32>)> {
        let mut requests = Vec::new();
        for command in sent(probe, 0x05) {
            let mut rest = &command[3..];
            while let [request, tail @ ..] = rest {
                if request & 0b10 != 0 {
                    requests.push((request & 0b1111, None));
                    rest = tail;
                } else {
                    let value = u32::from_le_bytes(tail[..4].try_into().unwrap());
                    requests.push((request & 0b1111, Some(value)));
                    rest = &tail[4..];
                }
            }
        }
        requests
    }

    /// Attaches over `protocol`, then returns the commands that a mix of AP accesses sends.
    fn ap_access_commands(protocol: WireProtocol) -> Vec<u8> {
        let (mut interface, probe) = attached(protocol, DPV2, |_| None);

        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        interface.read_raw_ap_register(&ap, 0xFC).unwrap();
        interface
            .write_raw_ap_register(&ap, 0x04, 0x2000_0000)
            .unwrap();
        interface
            .read_raw_ap_register_repeated(&ap, 0x0C, &mut [0; 32])
            .unwrap();
        interface
            .write_raw_ap_register_repeated(&ap, 0x0C, &[0; 32])
            .unwrap();
        interface
            .access_raw_ap_registers(&ap, &[(0x04, Some(0x2000_0000)), (0x0C, None)], &mut [0])
            .unwrap();

        probe.lock().unwrap().sent.iter().map(|c| c[0]).collect()
    }

    #[test]
    fn swd_ap_accesses_run_in_the_probe_firmware() {
        let commands = ap_access_commands(WireProtocol::Swd);
        assert!(
            commands.iter().all(|id| [0x05, 0x06].contains(id)),
            "{commands:x?}"
        );
        assert!(commands.contains(&0x06), "{commands:x?}");
    }

    #[test]
    fn jtag_ap_accesses_run_in_the_probe_firmware() {
        let commands = ap_access_commands(WireProtocol::Jtag);
        assert!(
            commands.iter().all(|id| [0x05, 0x06].contains(id)),
            "{commands:x?}"
        );
        assert!(commands.contains(&0x06), "{commands:x?}");
    }

    #[test]
    fn a_lost_link_is_connected_again_and_the_access_repeated() {
        // The first AP read after the attach gets no acknowledgement.
        let lose_link = Arc::new(AtomicBool::new(false));
        let armed = lose_link.clone();
        let (mut interface, probe) =
            attached(WireProtocol::Swd, DPV2, move |command| match command {
                [0x05, _, _, request, ..]
                    if request & 0b11 == 0b11 && armed.swap(false, Ordering::SeqCst) =>
                {
                    Some(Reply::Now(vec![0x05, 0, Ack::NoAck as u8]))
                }
                _ => None,
            });
        lose_link.store(true, Ordering::SeqCst);

        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        interface.read_raw_ap_register(&ap, 0xFC).unwrap();
        assert!(!lose_link.load(Ordering::SeqCst));
        // The line reset of the connect sequence.
        assert!(!sent(&probe, 0x12).is_empty());
    }

    #[test]
    fn a_dpv3_ap_select_writes_select1_before_select() {
        let (mut interface, probe) = attached(WireProtocol::Swd, DPV3, |_| None);

        let ap = FullyQualifiedApAddress::v2_with_default_dp(ApV2Address::new(0x1_0000_2000));
        interface.read_raw_ap_register(&ap, 0x0C).unwrap();
        assert_eq!(
            transfer_requests(&probe),
            [
                (0b1000, Some(0x2005)),
                (0b0100, Some(1)),
                (0b1000, Some(0x2000)),
                (0b1111, None),
            ]
        );
    }
}
