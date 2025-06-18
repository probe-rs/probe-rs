//! RISCV DTM based on the ARM Debug Interface (ADI)
//!
//! This is used in mixed architecture chips.
//!

use crate::{
    architecture::{
        arm::memory::ArmMemoryInterface,
        riscv::{
            communication_interface::{
                RiscvCommunicationInterface, RiscvDebugInterfaceState, RiscvError,
                RiscvInterfaceBuilder,
            },
            dtm::dtm_access::DtmAccess,
        },
    },
    probe::DeferredResultIndex,
};

type DtmState = ();

// TODO: Should this be the ArmDebugInterface?
pub struct AdiDtmBuilder<'probe>(Box<dyn ArmMemoryInterface + 'probe>);

impl<'probe> AdiDtmBuilder<'probe> {
    pub fn new(probe: Box<dyn ArmMemoryInterface + 'probe>) -> Self {
        Self(probe)
    }
}

pub fn interface_state() -> RiscvDebugInterfaceState {
    // We don't have any interface state currently
    RiscvDebugInterfaceState::new(Box::new(()))
}

impl<'probe> RiscvInterfaceBuilder<'probe> for AdiDtmBuilder<'probe> {
    fn create_state(&self) -> RiscvDebugInterfaceState {
        // We don't have any interface state currently
        RiscvDebugInterfaceState::new(Box::new(()))
    }

    fn attach<'state>(
        self: Box<Self>,
        state: &'state mut RiscvDebugInterfaceState,
    ) -> Result<RiscvCommunicationInterface<'state>, crate::probe::DebugProbeError>
    where
        'probe: 'state,
    {
        Ok(RiscvCommunicationInterface::new(
            Box::new(AdiDtm::new(self.0)),
            &mut state.interface_state,
        ))
    }
}

enum Command {
    Read(u32),
    Write(u32, u32),
}

/// Access to the Debug Transport Module (DTM),
/// which is used to communicate with the RISC-V debug module.
pub struct AdiDtm<'probe> {
    queued_reads: Vec<(DeferredResultIndex, u32)>,
    pub probe: Box<dyn ArmMemoryInterface + 'probe>,
}

impl<'probe> AdiDtm<'probe> {
    pub fn new(probe: Box<dyn ArmMemoryInterface + 'probe>) -> Self {
        Self {
            probe,
            queued_reads: Vec::new(),
        }
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
        todo!()
    }

    fn read_deferred_result(
        &mut self,
        index: crate::probe::DeferredResultIndex,
    ) -> Result<crate::probe::CommandResult, RiscvError> {
        todo!()
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        todo!()
    }

    fn schedule_write(
        &mut self,
        address: u64,
        value: u32,
    ) -> Result<Option<crate::probe::DeferredResultIndex>, RiscvError> {
        self.probe.write_word_32(address, value).unwrap();

        self.probe.flush().unwrap();

        Ok(None)
    }

    fn schedule_read(
        &mut self,
        address: u64,
    ) -> Result<crate::probe::DeferredResultIndex, RiscvError> {
        todo!()
    }

    fn read_with_timeout(
        &mut self,
        address: u64,
        timeout: std::time::Duration,
    ) -> Result<u32, RiscvError> {
        todo!()
    }

    fn write_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        timeout: std::time::Duration,
    ) -> Result<Option<u32>, RiscvError> {
        todo!()
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, crate::probe::DebugProbeError> {
        todo!()
    }
}
