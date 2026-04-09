use tokio_util::sync::CancellationToken;

use std::ops::ControlFlow;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{Core, CoreType, Error, HaltReason, VectorCatchCondition};

use crate::rpc::{ObjectStorage, SessionState};

pub struct RunLoop {
    pub core_id: usize,
    pub cancellation_token: CancellationToken,
}

/// Configuration for which vector catches to enable during the run loop.
#[derive(Debug, Clone, Copy, Default)]
pub struct VectorCatchConfig {
    pub catch_hardfault: bool,
    pub catch_reset: bool,
    pub catch_svc: bool,
    pub catch_hlt: bool,
}

#[derive(PartialEq, Debug)]
pub enum ReturnReason<R> {
    /// The predicate requested a return
    Predicate(R),
    /// Timeout elapsed
    Timeout,
    /// Cancelled
    Cancelled,
    /// The core locked up
    LockedUp,
}

impl RunLoop {
    /// Attaches to RTT and runs the core until it halts.
    ///
    /// Upon halt the predicate is invoked with the halt reason:
    /// * If the predicate returns `Ok(Some(r))` the run loop returns `Ok(ReturnReason::Predicate(r))`.
    /// * If the predicate returns `Ok(None)` the run loop will continue running the core.
    /// * If the predicate returns `Err(e)` the run loop will return `Err(e)`.
    ///
    /// The function will also return on timeout with `Ok(ReturnReason::Timeout)` or if the user presses CTRL + C with `Ok(ReturnReason::User)`.
    pub fn run_until<F, R>(
        &mut self,
        shared_session: &SessionState<'_>,
        vector_catch: VectorCatchConfig,
        mut poller: impl RunLoopPoller,
        timeout: Option<Duration>,
        mut predicate: F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let VectorCatchConfig {
            catch_hardfault,
            catch_reset,
            catch_svc,
            catch_hlt,
        } = vector_catch;

        // Prepare run loop
        {
            let mut session = shared_session.session_blocking();
            let mut core = session.core(self.core_id)?;
            let needs_vector_catch = catch_hardfault || catch_reset || catch_svc || catch_hlt;

            if needs_vector_catch {
                if !core.core_halted()? {
                    core.halt(Duration::from_millis(100))?;
                }

                // For ARMv7-A/R and ARMv8-A cores: if we're at the reset vector (PC = 0), step
                // past it first. This happens after reset_and_halt - enabling the reset catch
                // while at the reset vector causes an immediate halt.
                if catch_reset
                    && matches!(
                        core.core_type(),
                        CoreType::Armv7a | CoreType::Armv7r | CoreType::Armv8a
                    )
                {
                    let pc: u64 = core.read_core_reg(core.program_counter())?;
                    if pc == 0 {
                        core.step()?;
                    }
                }

                let catches = [
                    (catch_hardfault, VectorCatchCondition::HardFault),
                    (catch_reset, VectorCatchCondition::CoreReset),
                    (catch_svc, VectorCatchCondition::Svc),
                    (catch_hlt, VectorCatchCondition::Hlt),
                ];

                for (enabled, condition) in catches {
                    let result = if enabled {
                        core.enable_vector_catch(condition)
                    } else {
                        core.disable_vector_catch(condition)
                    };
                    match result {
                        Ok(_) | Err(Error::NotImplemented(_)) => {}
                        Err(e) => {
                            tracing::error!("Failed to set vector catch {:?}: {:?}", condition, e)
                        }
                    }
                }
            }

            let object_storage = shared_session.object_storage();
            poller.start(&object_storage, &mut core)?;

            if core.core_halted()? {
                core.run()?;
            }
        }

        let result = self.do_run_until(shared_session, &mut poller, timeout, &mut predicate);

        // Clean up run loop
        let mut session = shared_session.session_blocking();
        let mut core = session.core(self.core_id)?;
        let object_storage = shared_session.object_storage();
        // Always clean up after RTT but don't overwrite the original result.
        let poller_exit_result = poller.exit(&object_storage, &mut core);
        if result.is_ok() {
            // If the result is Ok, we return the potential error during cleanup.
            poller_exit_result?;
        }

        result
    }

    fn do_run_until<F, R>(
        &mut self,
        shared_session: &SessionState<'_>,
        poller: &mut impl RunLoopPoller,
        timeout: Option<Duration>,
        predicate: &mut F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let start = Instant::now();

        loop {
            match self.poll_once(shared_session, poller, predicate)? {
                ControlFlow::Break(reason) => return Ok(reason),
                ControlFlow::Continue(next_poll) => {
                    if let Some(timeout) = timeout
                        && start.elapsed() >= timeout
                    {
                        return Ok(ReturnReason::Timeout);
                    }

                    // If the polling frequency is too high, the USB connection to the probe
                    // can become unstable. Hence we only poll as little as necessary.
                    thread::sleep(next_poll);
                }
            }
        }
    }

    fn poll_once<F, R>(
        &self,
        shared_session: &SessionState<'_>,
        poller: &mut impl RunLoopPoller,
        predicate: &mut F,
    ) -> Result<ControlFlow<ReturnReason<R>, Duration>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let mut session = shared_session.session_blocking();
        let mut core = session.core(self.core_id)?;

        let mut next_poll = Duration::from_millis(100);
        let object_storage = shared_session.object_storage();

        // check for halt first, poll rtt after.
        // this is important so we do one last poll after halt, so we flush all messages
        // the core printed before halting, such as a panic message.
        let return_reason = match core.status()? {
            probe_rs::CoreStatus::Halted(reason) => match predicate(reason, &mut core) {
                Ok(Some(r)) => Some(Ok(ReturnReason::Predicate(r))),
                Err(e) => Some(Err(e)),
                Ok(None) => {
                    // Re-poll immediately if the core was halted, to speed up reading strings
                    // from semihosting. The core is not expected to be halted for other reasons.
                    next_poll = Duration::ZERO;
                    core.run()?;
                    None
                }
            },
            probe_rs::CoreStatus::Running
            | probe_rs::CoreStatus::Sleeping
            | probe_rs::CoreStatus::Unknown => {
                // Carry on
                None
            }

            probe_rs::CoreStatus::LockedUp => Some(Ok(ReturnReason::LockedUp)),
        };

        let poller_result = poller.poll(&object_storage, &mut core);

        if let Some(reason) = return_reason {
            return reason.map(ControlFlow::Break);
        }
        if self.cancellation_token.is_cancelled() {
            return Ok(ControlFlow::Break(ReturnReason::Cancelled));
        }
        match poller_result {
            Ok(delay) => next_poll = next_poll.min(delay),
            Err(error) => return Err(error),
        }

        Ok(ControlFlow::Continue(next_poll))
    }
}

pub trait RunLoopPoller {
    fn start(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<()>;
    fn poll(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<Duration>;
    fn exit(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<()>;
}

pub struct NoopPoller;

impl RunLoopPoller for NoopPoller {
    fn start(&mut self, _: &ObjectStorage, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }

    fn poll(&mut self, _: &ObjectStorage, _core: &mut Core<'_>) -> Result<Duration> {
        Ok(Duration::from_secs(u64::MAX))
    }

    fn exit(&mut self, _: &ObjectStorage, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }
}

impl<T> RunLoopPoller for Option<T>
where
    T: RunLoopPoller,
{
    fn start(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<()> {
        if let Some(poller) = self {
            poller.start(objs, core)
        } else {
            NoopPoller.start(objs, core)
        }
    }

    fn poll(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<Duration> {
        if let Some(poller) = self {
            poller.poll(objs, core)
        } else {
            NoopPoller.poll(objs, core)
        }
    }

    fn exit(&mut self, objs: &ObjectStorage, core: &mut Core<'_>) -> Result<()> {
        if let Some(poller) = self {
            poller.exit(objs, core)
        } else {
            NoopPoller.exit(objs, core)
        }
    }
}
