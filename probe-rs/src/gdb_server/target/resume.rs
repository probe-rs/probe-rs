use super::{ResumeAction, RuntimeTarget};

use gdbstub::target::ext::base::multithread::MultiThreadSingleStepOps;
use gdbstub::target::ext::base::multithread::{MultiThreadResume, MultiThreadSingleStep};

impl MultiThreadResume for RuntimeTarget<'_> {
    fn resume(&mut self) -> Result<(), Self::Error> {
        let mut session = self.session.lock().unwrap();

        match self.resume_action {
            (_, ResumeAction::Resume) => {
                for core_id in self.cores.iter() {
                    let mut core = session.core(*core_id)?;
                    core.run()?;
                }
            }
            (core_id, ResumeAction::Step) => {
                let mut core = session.core(core_id)?;
                core.step()?;
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

impl MultiThreadSingleStep for RuntimeTarget<'_> {
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
