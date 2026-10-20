//! CMSIS-DAP as an ARM debug interface. The probe firmware runs the DP and AP accesses.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use crate::{
    CoreStatus,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, DapError, FullyQualifiedApAddress, Pins,
        RegisterAddress, SwdSequence, SwoAccess, SwoConfig,
        ap::MemoryAp,
        communication_interface::{DpHost, DpManager, probe_debug_port_wire, raw_pin_levels},
        dp::{Abort, Ctrl, DpAddress, DpRegister, DpRegisterAddress},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
        traits::DebugPortWire,
    },
    probe::{
        BatchError, BitSequence, DebugProbe, DebugProbeError, JtagChain, Probe, WireProtocol,
        cmsisdap::CmsisDap,
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

        let mut batch = SwdBatch::new();
        let mut reads = Vec::new();
        for &(port, addr, value) in accesses {
            match value {
                Some(value) => batch.write(port, addr, value),
                None => reads.push(batch.read(port, addr)),
            }
        }
        // Over JTAG, a failed AP access is acknowledged as OK, and only CTRL/STAT shows it.
        let ctrl_addr = RegisterAddress::from(Ctrl::ADDRESS).a2_and_3();
        let ctrl = (jtag && accesses.iter().any(|(port, ..)| *port == Port::Ap))
            .then(|| batch.read(Port::Dp, ctrl_addr));

        let mut results = self.run_transfers(&batch)?;
        for read in reads {
            *values.next().unwrap() = results.take(read).unwrap();
        }

        if let Some(ctrl) = ctrl {
            let ctrl = Ctrl(results.take(ctrl).unwrap());
            if ctrl.sticky_err() {
                tracing::debug!("JTAG transfers failed, CTRL/STAT: {ctrl:?}");
                // A JTAG-DP clears its sticky flags when they are written as one.
                let mut clear = SwdBatch::new();
                clear.write(Port::Dp, ctrl_addr, ctrl.into());
                self.run_transfers(&clear)?;
                return Err(DapError::FaultResponse.into());
            }
        }
        Ok(())
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
        Err(ArmError::Dap(match error {
            SwdTransferError::NoAcknowledge => DapError::NoAcknowledge,
            SwdTransferError::WaitResponse => DapError::WaitResponse,
            SwdTransferError::FaultResponse => DapError::FaultResponse,
            SwdTransferError::IncorrectParity => DapError::IncorrectParity,
            SwdTransferError::Protocol => {
                DapError::Protocol(self.protocol.unwrap_or(WireProtocol::Swd))
            }
        }))
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

/// An ARM debug interface on a CMSIS-DAP probe.
#[derive(Debug)]
pub(crate) struct CmsisDapArmDebug {
    probe: Option<Box<CmsisDap>>,
    dp_manager: DpManager,
}

impl CmsisDapArmDebug {
    pub(crate) fn new(probe: Box<CmsisDap>, sequence: Arc<dyn ArmDebugSequence>) -> Self {
        Self {
            probe: Some(probe),
            // The probe firmware retries a WAIT, so no overrun detection is necessary.
            dp_manager: DpManager::new(sequence, false),
        }
    }

    fn probe(&mut self) -> &mut CmsisDap {
        self.probe.as_mut().unwrap()
    }

    fn protocol(&self) -> Option<WireProtocol> {
        self.probe.as_ref().unwrap().protocol
    }

    /// Select `ap` and the bank of `address`, then run the accesses on the AP.
    fn ap_transfer(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        accesses: impl IntoIterator<Item = (u64, Option<u32>)>,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        self.select_ap_and_ap_bank(ap, address)?;
        let ap_addr = |address: u64| RegisterAddress::ApRegister(address as u8).a2_and_3();
        let accesses: Vec<Access> = accesses
            .into_iter()
            .map(|(address, value)| (Port::Ap, ap_addr(address), value))
            .collect();
        self.probe().transfer(&accesses, values)
    }
}

impl Drop for CmsisDapArmDebug {
    fn drop(&mut self) {
        if self.probe.is_some() {
            self.disconnect();
            self.probe = None;
        }
    }
}

impl DpHost for CmsisDapArmDebug {
    fn dp_manager(&mut self) -> &mut DpManager {
        &mut self.dp_manager
    }

    fn with_debug_port_wire<R>(
        &mut self,
        f: impl FnOnce(&mut dyn DebugPortWire) -> Result<R, ArmError>,
    ) -> Result<R, ArmError> {
        f(&mut CmsisDapWire(self.probe()))
    }
}

impl ArmDebugInterface for CmsisDapArmDebug {
    fn reinitialize(&mut self) -> Result<(), ArmError> {
        self.reinitialize_dps()
    }

    fn access_ports(
        &mut self,
        dp: DpAddress,
    ) -> Result<BTreeSet<FullyQualifiedApAddress>, ArmError> {
        self.dp_access_ports(dp)
    }

    fn close(mut self: Box<Self>) -> Probe {
        self.disconnect();
        Probe::from_attached_probe(self.probe.take().unwrap())
    }

    fn select_debug_port(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        self.select_dp(dp)?;
        Ok(())
    }

    fn current_debug_port(&self) -> Option<DpAddress> {
        self.dp_manager.current_dp
    }

    fn memory_interface(
        &mut self,
        access_port: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + '_>, ArmError> {
        self.adi_memory_interface(access_port)
    }

    fn core_status_notification(&mut self, state: CoreStatus) {
        DebugProbe::core_status_notification(self.probe(), state).ok();
    }

    fn active_wire_protocol(&self) -> Option<WireProtocol> {
        self.protocol()
    }

    fn wire_speed_khz(&self) -> Option<u32> {
        Some(self.probe.as_ref().unwrap().speed_khz)
    }

    fn set_wire_speed(&mut self, speed_khz: u32) -> Result<u32, ArmError> {
        DebugProbe::set_speed(self.probe(), speed_khz).map_err(ArmError::Probe)
    }
}

impl DapAccess for CmsisDapArmDebug {
    fn read_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
    ) -> Result<u32, ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let addr = RegisterAddress::DpRegister(address).a2_and_3();
        let mut value = [0];
        self.probe()
            .transfer(&[(Port::Dp, addr, None)], &mut value)?;
        Ok(value[0])
    }

    fn write_raw_dp_register(
        &mut self,
        dp: DpAddress,
        address: DpRegisterAddress,
        value: u32,
    ) -> Result<(), ArmError> {
        self.select_dp_and_dp_bank(dp, &address)?;
        let addr = RegisterAddress::DpRegister(address).a2_and_3();
        let result = self
            .probe()
            .transfer(&[(Port::Dp, addr, Some(value))], &mut []);
        self.sync_select_cache(dp, address, value, result.is_ok());
        result
    }

    fn read_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
    ) -> Result<u32, ArmError> {
        self.retry_after_link_loss(ap.dp(), |this| {
            let mut value = [0];
            this.ap_transfer(ap, address, [(address, None)], &mut value)?;
            Ok(value[0])
        })
    }

    fn read_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let accesses = vec![(address, None); values.len()];
        self.ap_transfer(ap, address, accesses, values)
    }

    fn access_raw_ap_registers(
        &mut self,
        ap: &FullyQualifiedApAddress,
        accesses: &[(u64, Option<u32>)],
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let Some(&(first, _)) = accesses.first() else {
            return Ok(());
        };
        // The bank is selected once, from the first address.
        assert!(
            accesses
                .iter()
                .all(|&(address, _)| address >> 4 == first >> 4),
            "every access has to be in the same AP register bank"
        );
        if accesses.iter().any(|(_, value)| value.is_some()) {
            self.forget_memory_ap(ap);
        }
        self.ap_transfer(ap, first, accesses.iter().copied(), values)
    }

    fn write_raw_ap_register(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        self.forget_memory_ap(ap);
        self.retry_after_link_loss(ap.dp(), |this| {
            this.ap_transfer(ap, address, [(address, Some(value))], &mut [])
        })
    }

    fn write_raw_ap_register_repeated(
        &mut self,
        ap: &FullyQualifiedApAddress,
        address: u64,
        values: &[u32],
    ) -> Result<(), ArmError> {
        self.forget_memory_ap(ap);
        let accesses = values.iter().map(|&value| (address, Some(value)));
        self.ap_transfer(ap, address, accesses, &mut [])
    }

    fn active_wire_protocol(&self) -> Option<WireProtocol> {
        self.protocol()
    }

    fn debug_port_reconnect_with(
        &mut self,
        connect: &mut dyn FnMut(&mut dyn DebugPortWire) -> Result<(), ArmError>,
    ) -> Result<(), ArmError> {
        self.forget_memory_aps();
        self.with_debug_port_wire(|wire| connect(wire))
    }

    fn memory_ap_epoch(&self) -> u64 {
        self.dp_manager.memory_ap_epoch
    }

    fn take_memory_ap(&mut self, ap: &FullyQualifiedApAddress) -> Option<MemoryAp> {
        self.dp_manager.take_memory_ap(ap)
    }

    fn store_memory_ap(&mut self, memory_ap: MemoryAp) {
        self.dp_manager.store_memory_ap(memory_ap);
    }
}

fn wire_probe_error(error: ArmError) -> DebugProbeError {
    match error {
        ArmError::Probe(error) => error,
        error => DebugProbeError::Other(error.to_string()),
    }
}

impl SwdSequence for CmsisDapArmDebug {
    fn swj_sequence(&mut self, bits: &BitSequence) -> Result<(), DebugProbeError> {
        self.with_debug_port_wire(|wire| wire.swj_sequence(bits))
            .map_err(wire_probe_error)
    }

    fn swj_pins(&mut self, out: u32, select: u32, wait: u32) -> Result<u32, DebugProbeError> {
        let levels = self
            .with_debug_port_wire(|wire| {
                let wait = Duration::from_micros(wait.into());
                wire.swj_pins(Pins(out as u8), Pins(select as u8), wait)
            })
            .map_err(wire_probe_error)?;
        Ok(raw_pin_levels(levels))
    }
}

impl SwoAccess for CmsisDapArmDebug {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        self.probe().enable_swo(config)
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        self.probe().disable_swo()
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        self.probe().read_swo_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
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
    fn a_changed_chain_runs_the_accesses_as_scans() {
        let (mut dap, _probe) = jtag_dap(0);

        let _ = CmsisDapWire(&mut dap).try_jtag_chain();
        assert!(!dap.jtag_configured);
    }
}
