use std::time::{Duration, Instant};

use super::{ResumeAction, RuntimeTarget};
use probe_rs_rpc::core_ops::{WireCoreStatus, WireHaltReason, WireSteppingMode};

use gdbstub::target::ext::base::multithread::{
    MultiThreadResume, MultiThreadSchedulerLocking, MultiThreadSchedulerLockingOps,
    MultiThreadSingleStep, MultiThreadSingleStepOps,
};

/// Max time to wait for a core to leave halt after resuming before we poll it.
const RESUME_SETTLE_TIMEOUT: Duration = Duration::from_millis(100);

/// Whether a halt reason reflects a real debug event (as opposed to a stale halt).
fn is_debug_event(reason: WireHaltReason) -> bool {
    matches!(
        reason,
        WireHaltReason::Breakpoint(_)
            | WireHaltReason::Step
            | WireHaltReason::Exception
            | WireHaltReason::Watchpoint
            | WireHaltReason::Multiple
    )
}

impl MultiThreadResume for RuntimeTarget {
    fn resume(&mut self) -> Result<(), Self::Error> {
        // Build the effective per-core action list. Cores with an explicit action
        // from GDB use it. Cores without one are continued by default unless GDB
        // requested scheduler locking, in which case they are left frozen.
        let actions: Vec<(usize, ResumeAction)> = self
            .cores
            .iter()
            .filter_map(|core| match self.resume_actions.get(&core.index) {
                Some(action) => Some((core.index, *action)),
                None if !self.scheduler_locked => Some((core.index, ResumeAction::Resume)),
                None => None,
            })
            .collect();

        // Track which cores actually started executing (or were single-stepped)
        // so the poll loop in `mod.rs` only scans those for a stop reason.
        self.running_cores.clear();

        // Emit the run-control decision so scheduler-locking is observable and
        // testable. `actions` shows each core's id and whether it is resumed or
        // single-stepped (a bare "resumed_cores" would be misleading for steps).
        tracing::debug!(
            target: "probe_rs_gdb_resume",
            "GDB resume: scheduler_locked={} actions={actions:?}",
            self.scheduler_locked,
        );

        let mut resumed = Vec::new();
        for (core_id, action) in actions {
            match action {
                ResumeAction::Resume => resumed.push(core_id as u32),
                ResumeAction::Step => {
                    self.block_on(
                        self.session
                            .debug_step(core_id as u32, WireSteppingMode::StepInstruction),
                    )?;
                    // A single-stepped core halts again with `Step`; it is a
                    // candidate for the stop reason (reported as SIGTRAP on its tid).
                    self.running_cores.push(core_id);
                }
            }
        }

        if resumed.is_empty() {
            return Ok(());
        }

        let mut statuses = self
            .block_on(self.session.resume_cores(Some(resumed.clone())))?
            .statuses;

        // A resumed core may still read halted briefly; wait for it to resume so
        // the poll loop doesn't misread that as a stop (#3965).
        let start = Instant::now();
        while statuses
            .iter()
            .any(|(_, status)| matches!(status, WireCoreStatus::Halted(r) if !is_debug_event(*r)))
            && start.elapsed() < RESUME_SETTLE_TIMEOUT
        {
            std::thread::sleep(Duration::from_millis(1));
            statuses = self
                .block_on(self.session.cores_status(Some(resumed.clone())))?
                .statuses;
        }

        // Include the core as "running" if it actually left halt, or if it halted
        // again immediately for a real debug event (a breakpoint/watchpoint/exception
        // at or near the resume PC). Only stale, non-debug halt reasons are excluded,
        // so a core that never actually resumed (e.g. clock-gated in reset) cannot
        // hijack the stop with a spurious SIGINT, while a genuine early stop is still
        // reported instead of hanging GDB.
        for (index, status) in statuses {
            let is_running = match status {
                WireCoreStatus::Halted(reason) => is_debug_event(reason),
                _ => true,
            };
            if is_running {
                self.running_cores.push(index as usize);
            }
        }

        Ok(())
    }

    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        self.resume_actions.clear();
        self.scheduler_locked = false;
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        tid: gdbstub::common::Tid,
        _signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
        let core_id = tid.get() - 1;
        self.resume_actions.insert(core_id, ResumeAction::Resume);
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<MultiThreadSingleStepOps<'_, Self>> {
        Some(self)
    }

    fn support_scheduler_locking(&mut self) -> Option<MultiThreadSchedulerLockingOps<'_, Self>> {
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
        self.resume_actions.insert(core_id, ResumeAction::Step);
        Ok(())
    }
}

impl MultiThreadSchedulerLocking for RuntimeTarget {
    fn set_resume_action_scheduler_lock(&mut self) -> Result<(), Self::Error> {
        // GDB requested `set scheduler-locking on`: only the cores with an
        // explicit resume action may run; every other core must stay frozen.
        self.scheduler_locked = true;
        Ok(())
    }
}
