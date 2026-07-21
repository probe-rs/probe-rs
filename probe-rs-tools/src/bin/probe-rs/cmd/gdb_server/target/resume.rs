use super::{ResumeAction, RuntimeTarget};

use gdbstub::target::ext::base::multithread::MultiThreadSingleStepOps;
use gdbstub::target::ext::base::multithread::{MultiThreadResume, MultiThreadSingleStep};
use probe_rs::CoreStatus;

impl MultiThreadResume for RuntimeTarget<'_> {
    fn resume(&mut self) -> Result<(), Self::Error> {
        let mut session = self.session.lock();

        match self.resume_action {
            (_, ResumeAction::Resume) => {
                for core_id in self.cores.iter() {
                    let mut core = session.core(*core_id)?;
                    core.run()?;

                    // `run()` returns as soon as it clears the halt bit, but the core can
                    // still report halted for a moment afterwards. If the running-poll reads
                    // that stale halted state it looks like an unexpected stop and GDB gets a
                    // spurious SIGINT (#3965). Give the core a few reads to actually get going
                    // before we hand control back to the poll loop. Bounded, so a core that
                    // genuinely re-halts immediately (e.g. a breakpoint on the next
                    // instruction) still falls through and gets reported normally.
                    for _ in 0..10 {
                        if !matches!(core.status()?, CoreStatus::Halted(_)) {
                            break;
                        }
                    }
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
