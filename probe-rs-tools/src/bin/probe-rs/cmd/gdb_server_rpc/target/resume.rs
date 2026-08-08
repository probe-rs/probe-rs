use std::time::{Duration, Instant};

use super::{ResumeAction, RuntimeTarget, is_core_disabled};
use probe_rs_rpc::core_ops::{WireCoreStatus, WireSteppingMode};

use gdbstub::target::ext::base::multithread::MultiThreadSingleStepOps;
use gdbstub::target::ext::base::multithread::{MultiThreadResume, MultiThreadSingleStep};

/// Max time to wait for a core to leave halt after resuming before we poll it.
const RESUME_SETTLE_TIMEOUT: Duration = Duration::from_millis(100);

impl MultiThreadResume for RuntimeTarget {
    fn resume(&mut self) -> Result<(), Self::Error> {
        match self.resume_action {
            (_, ResumeAction::Resume) => {
                for core_info in self.cores.clone() {
                    let core = self.session.core(core_info.index);
                    self.block_on(core.run())?;

                    let start = Instant::now();
                    loop {
                        let status = match self.block_on(core.status()) {
                            Ok(status) => status,
                            Err(error) if is_core_disabled(&error) => break,
                            Err(error) => return Err(error.into()),
                        };
                        if !matches!(status, WireCoreStatus::Halted(_)) {
                            break;
                        }
                        if start.elapsed() >= RESUME_SETTLE_TIMEOUT {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
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
