use std::time::{Duration, Instant};

use crate::architecture::riscv::communication_interface::{
    RiscvCommunicationInterface, RiscvDebugInterfaceState, RiscvError, RiscvInterfaceBuilder,
};
use crate::architecture::riscv::dtm::DtmAccess;
use crate::architecture::riscv::dtm::jtag_dtm::{
    DmiOperation, DmiOperationError, DmiOperationStatus,
};
use crate::probe::queue::{Handle, HandleId, Results};
use crate::probe::wlink::WchLink;
use crate::probe::{CommandResult, DebugProbe, DebugProbeError};

/// Hardcoded IDCODE, the same as WCH's OpenOCD fork.
const WCH_LINK_IDCODE: u32 = 0x00000001;

/// Number of address bits in the DMI register.
const WCH_LINK_DTMCS_ABITS: u32 = 7;

/// RISC-V debug specification version implemented by the probe (1 = 0.13).
const WCH_LINK_DTMCS_VERSION: u32 = 1;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct WchLinkDtmState {
    pending: Vec<(Option<HandleId>, DmiOperation)>,
    results: Results,
    abits: u32,
}

/// Object that can be used to build a RISC-V DTM interface from a WCH-Link probe.
pub struct WchLinkDtmBuilder<'f>(&'f mut WchLink);

impl<'f> WchLinkDtmBuilder<'f> {
    /// Create a new DTM builder via a WCH-Link probe.
    pub fn new(probe: &'f mut WchLink) -> Self {
        Self(probe)
    }
}

impl<'probe> RiscvInterfaceBuilder<'probe> for WchLinkDtmBuilder<'probe> {
    fn create_state(&self) -> RiscvDebugInterfaceState {
        RiscvDebugInterfaceState::new(Box::<WchLinkDtmState>::default())
    }

    fn attach<'state>(
        self: Box<Self>,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, DebugProbeError>
    where
        'probe: 'state,
    {
        let dtm_state = state.dtm_state.downcast_mut::<WchLinkDtmState>().unwrap();

        Ok(RiscvCommunicationInterface::new(
            Box::new(WchLinkDtm::new(self.0, dtm_state)),
            &mut state.interface_state,
        ))
    }
}

#[derive(Debug)]
struct WchLinkDtm<'probe> {
    probe: &'probe mut WchLink,
    state: &'probe mut WchLinkDtmState,
}

impl<'probe> WchLinkDtm<'probe> {
    fn new(probe: &'probe mut WchLink, state: &'probe mut WchLinkDtmState) -> Self {
        Self { probe, state }
    }

    fn transform_dmi_response(data: u32, op: u8) -> Result<u32, DmiOperationError> {
        let status = DmiOperationStatus::parse(op).expect("INVALID DMI OP status");

        match status {
            DmiOperationStatus::Ok => Ok(data),
            DmiOperationStatus::Reserved => Err(DmiOperationError::Reserved),
            DmiOperationStatus::RequestInProgress => Err(DmiOperationError::RequestInProgress),
            DmiOperationStatus::OperationFailed => Err(DmiOperationError::OperationFailed),
        }
    }

    fn perform_dmi_operation(
        &mut self,
        op: DmiOperation,
    ) -> Result<Result<u32, DmiOperationError>, DebugProbeError> {
        let (_addr, data, status) = match op {
            DmiOperation::Read { address } => {
                let response = self.probe.dmi_op_read(address as u8)?;
                tracing::trace!(
                    "dmi read 0x{:02x} 0x{:08x} op={}",
                    response.0,
                    response.1,
                    response.2
                );
                self.probe.set_last_dmi_read(response);
                response
            }
            DmiOperation::NoOp => {
                // No idea why NOP with zero addr should return the last read value.
                // see-also: RiscvCommunicationInterface::read_dm_register_untyped
                let response = match self.probe.last_dmi_read() {
                    Some(cached) => cached,
                    None => self.probe.dmi_op_nop()?,
                };
                tracing::trace!(
                    "dmi nop 0x{:02x} 0x{:08x} op={}",
                    response.0,
                    response.1,
                    response.2
                );
                response
            }
            DmiOperation::Write { address, value } => {
                let response = self.probe.dmi_op_write(address as u8, value)?;
                tracing::trace!(
                    "dmi write 0x{:02x} 0x{:08x} op={}",
                    response.0,
                    response.1,
                    response.2
                );
                if address == 0x10 && value == 0x40000001 {
                    // needs additional sleep for a resume operation
                    std::thread::sleep(Duration::from_millis(10));
                }
                response
            }
        };

        Ok(Self::transform_dmi_response(data, status))
    }

    fn dmi_operation_with_retry(
        &mut self,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        let start_time = Instant::now();

        loop {
            match self.perform_dmi_operation(op)? {
                Ok(result) => return Ok(result),
                Err(DmiOperationError::RequestInProgress) => {
                    self.clear_error_state()?;
                }
                Err(DmiOperationError::Reserved | DmiOperationError::OperationFailed) => {
                    self.clear_error_state()?;
                    return Err(RiscvError::DtmOperationFailed);
                }
            }

            if start_time.elapsed() > timeout {
                return Err(RiscvError::Timeout);
            }
        }
    }

    fn dmi_operation_with_timeout(
        &mut self,
        op: DmiOperation,
        timeout: Duration,
    ) -> Result<u32, RiscvError> {
        self.execute()?;
        self.dmi_operation_with_retry(op, timeout)
    }
}

impl DtmAccess for WchLinkDtm<'_> {
    fn init(&mut self) -> Result<(), RiscvError> {
        if WCH_LINK_DTMCS_VERSION != 1 {
            return Err(RiscvError::UnsupportedDebugTransportModuleVersion(
                WCH_LINK_DTMCS_VERSION as u8,
            ));
        }

        self.state.abits = WCH_LINK_DTMCS_ABITS;

        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_assert()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.probe.target_reset_deassert()
    }

    fn clear_error_state(&mut self) -> Result<(), RiscvError> {
        tracing::debug!("DMI reset");
        self.probe.dmi_op_write(0x10, 0x00000000)?;
        self.probe.dmi_op_write(0x10, 0x00000001)?;
        Ok(())
    }

    fn read_deferred_result(
        &mut self,
        index: Handle<CommandResult>,
    ) -> Result<CommandResult, RiscvError> {
        match self.state.results.take(index) {
            Ok(result) => Ok(result),
            Err(handle) => {
                self.execute()?;
                self.state
                    .results
                    .take(handle)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)
            }
        }
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        for (id, op) in std::mem::take(&mut self.state.pending) {
            let result = self.dmi_operation_with_retry(op, COMMAND_TIMEOUT)?;
            if let Some(handle_id) = id {
                self.state
                    .results
                    .push(&handle_id, CommandResult::U32(result));
            }
        }

        Ok(())
    }

    fn schedule_write(
        &mut self,
        address: u64,
        value: u32,
    ) -> Result<Option<Handle<CommandResult>>, RiscvError> {
        let id = HandleId::new();
        self.state
            .pending
            .push((Some(id.clone()), DmiOperation::Write { address, value }));
        Ok(Some(Handle::from_id(id)))
    }

    fn schedule_read(&mut self, address: u64) -> Result<Handle<CommandResult>, RiscvError> {
        self.state
            .pending
            .push((None, DmiOperation::Read { address }));

        let id = HandleId::new();
        self.state
            .pending
            .push((Some(id.clone()), DmiOperation::NoOp));
        Ok(Handle::from_id(id))
    }

    fn read_with_timeout(&mut self, address: u64, timeout: Duration) -> Result<u32, RiscvError> {
        self.state
            .pending
            .push((None, DmiOperation::Read { address }));

        self.dmi_operation_with_timeout(DmiOperation::NoOp, timeout)
    }

    fn write_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        timeout: Duration,
    ) -> Result<Option<u32>, RiscvError> {
        self.dmi_operation_with_timeout(DmiOperation::Write { address, value }, timeout)
            .map(Some)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        tracing::debug!("using hard coded idcode 0x00000001");
        Ok(Some(WCH_LINK_IDCODE))
    }
}
