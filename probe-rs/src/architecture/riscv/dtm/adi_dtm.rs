//! RISCV DTM based on the ARM Debug Interface (ADI)
//!
//! This is used in mixed architecture chips.
//!

use crate::{
    architecture::{
        arm::{ArmError, memory::ArmMemoryInterface},
        riscv::{
            communication_interface::{
                MemoryAccessMethod, RiscvCommunicationInterface, RiscvDebugInterfaceState,
                RiscvError, RiscvInterfaceBuilder,
            },
            dtm::dtm_access::{DmAddress, DtmAccess},
        },
    },
    probe::{CommandQueue, CommandResult, DeferredResultSet},
};

#[derive(Default)]
pub struct DtmState {
    queued_reads: CommandQueue<Command>,
    result_set: DeferredResultSet<(DmAddress, Result<u32, ArmError>)>,

    offset: u64,
}

// TODO: Should this be the ArmDebugInterface?
pub struct AdiDtmBuilder<'probe> {
    probe: Box<dyn ArmMemoryInterface + 'probe>,
    offset: Option<u64>,
}

impl<'probe> AdiDtmBuilder<'probe> {
    pub fn new(probe: Box<dyn ArmMemoryInterface + 'probe>, offset: Option<u64>) -> Self {
        Self { probe, offset }
    }
}

impl<'probe> RiscvInterfaceBuilder<'probe> for AdiDtmBuilder<'probe> {
    fn create_state(&self) -> RiscvDebugInterfaceState {
        let mut state = DtmState::default();

        state.offset = self.offset.unwrap_or(0);

        RiscvDebugInterfaceState::new(Box::new(state), Some(MemoryAccessMethod::Dtm))
    }

    fn attach<'state>(
        self: Box<Self>,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, crate::probe::DebugProbeError>
    where
        'probe: 'state,
    {
        let dtm_state = state.dtm_state.downcast_mut::<DtmState>().unwrap();

        Ok(RiscvCommunicationInterface::new(
            Box::new(AdiDtm::new(self.probe, dtm_state)),
            &mut state.interface_state,
        ))
    }
}

enum Command {
    Read(DmAddress),
}

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
pub struct AdiDtm<'probe> {
    pub probe: Box<dyn ArmMemoryInterface + 'probe>,
    state: &'probe mut DtmState,
}

impl<'probe> AdiDtm<'probe> {
    pub fn new(probe: Box<dyn ArmMemoryInterface + 'probe>, state: &'probe mut DtmState) -> Self {
        Self { probe, state }
    }
}

impl DtmAccess for AdiDtm<'_> {
    fn target_reset_assert(&mut self) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), crate::probe::DebugProbeError> {
        todo!()
    }

    fn clear_error_state(&mut self) -> Result<(), RiscvError> {
        // TODO: The ARM debug interface has an error state,
        // maybe this should be a reconnect?
        Ok(())
    }

    fn read_deferred_result(
        &mut self,
        index: crate::probe::DeferredResultIndex,
    ) -> Result<crate::probe::CommandResult, RiscvError> {
        let (address, result) = match self.state.result_set.take(index) {
            Ok(value) => value,
            Err(index) => {
                self.execute()?;

                let value = self
                    .state
                    .result_set
                    .take(index)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)?;

                value
            }
        };

        let value = result.map_err(|e| RiscvError::DmReadFailed {
            address: address.0,
            source: Box::new(e),
        })?;

        Ok(CommandResult::U32(value))
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        let cmds = std::mem::take(&mut self.state.queued_reads);

        for (index, cmd) in cmds.iter() {
            match cmd {
                Command::Read(address) => {
                    let byte_address = u64::from(address.0 * 4) + self.state.offset;

                    let result = self.probe.read_word_32(byte_address);

                    self.state.result_set.push(index, (*address, result));
                }
            }
        }

        Ok(())
    }

    fn schedule_write(
        &mut self,
        address: DmAddress,
        value: u32,
    ) -> Result<Option<crate::probe::DeferredResultIndex>, RiscvError> {
        // The ArmMemoryInterface is byte addressed, while the DmAddress is word addressed.
        let mapped_address = u64::from(address.0 * 4) + self.state.offset;

        self.probe
            .write_word_32(mapped_address, value)
            .map_err(|e| RiscvError::DmWriteFailed {
                address: address.0,
                source: Box::new(e),
            })?;

        self.probe.flush().unwrap();

        Ok(None)
    }

    fn schedule_read(
        &mut self,
        address: DmAddress,
    ) -> Result<crate::probe::DeferredResultIndex, RiscvError> {
        Ok(self.state.queued_reads.schedule(Command::Read(address)))
    }

    fn read_with_timeout(
        &mut self,
        _address: DmAddress,
        _timeout: std::time::Duration,
    ) -> Result<u32, RiscvError> {
        todo!()
    }

    fn write_with_timeout(
        &mut self,
        address: DmAddress,
        value: u32,
        _timeout: std::time::Duration,
    ) -> Result<Option<u32>, RiscvError> {
        let addr = u64::from(address.0 * 4) + self.state.offset;

        self.probe
            .write_word_32(addr, value)
            .map_err(|e| RiscvError::DmWriteFailed {
                address: address.0,
                source: Box::new(e),
            })?;

        Ok(None)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, crate::probe::DebugProbeError> {
        Ok(None)
    }

    fn memory_interface<'m>(
        &'m mut self,
    ) -> Result<&'m mut dyn crate::MemoryInterface<ArmError>, crate::probe::DebugProbeError> {
        let arm_interface: &mut dyn ArmMemoryInterface = self.probe.as_mut();

        Ok(arm_interface)
    }
}
