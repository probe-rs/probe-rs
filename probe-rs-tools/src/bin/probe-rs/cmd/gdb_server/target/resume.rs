use super::{ResumeAction, RuntimeTarget};
use probe_rs_rpc::core_ops::WireSteppingMode;

use gdbstub::target::ext::base::multithread::MultiThreadSingleStepOps;
use gdbstub::target::ext::base::multithread::{MultiThreadResume, MultiThreadSingleStep};

impl MultiThreadResume for RuntimeTarget {
    fn resume(&mut self) -> Result<(), Self::Error> {
        match self.resume_action {
            (_, ResumeAction::Resume) => {
                let cores = self.cores.iter().map(|core| core.index as u32).collect();
                self.block_on(self.session.resume_cores(Some(cores)))?;
            }
            (core_id, ResumeAction::Step) => {
                self.block_on(
                    self.session
                        .debug_step(core_id as u32, WireSteppingMode::StepInstruction),
                )?;
            }
            (_, ResumeAction::Unchanged) => {}
        }

        Ok(())
    }

    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        self.resume_action = (0, ResumeAction::Resume);
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        tid: gdbstub::common::Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        let core_id = tid.get() - 1;
        self.resume_action = (core_id, ResumeAction::Resume);
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl MultiThreadSingleStep for RuntimeTarget {
    fn set_resume_action_step(
        &mut self,
        tid: gdbstub::common::Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        let core_id = tid.get() - 1;
        self.resume_action = (core_id, ResumeAction::Step);
        Ok(())
    }
}
