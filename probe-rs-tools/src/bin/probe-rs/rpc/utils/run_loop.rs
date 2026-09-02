use tokio_util::sync::CancellationToken;

use std::ops::ControlFlow;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{Core, CoreType, Error, HaltReason, VectorCatchCondition};

use crate::rpc::SessionState;

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

/// Default interval between status polls of the primary core when RTT does not request a faster poll.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Interval between status polls of other cores. Longer than the primary interval to limit probe traffic.
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

impl RunLoop {
    /// Attaches to RTT and runs the primary core until it, or another enabled core, halts.
    ///
    /// Vector catch, the initial resume, and the poller run only on [`Self::core_id`]. Other cores
    /// are observed with `status()` only. A disabled hart is skipped and retried later. An
    /// unexpected halt, lock-up, or semihosting result on any observed core uses the same predicate
    /// as the primary core.
    ///
    /// Upon halt the predicate is invoked with the halt reason:
    /// * If the predicate returns `Ok(Some(r))` the run loop returns `Ok(ReturnReason::Predicate(r))`.
    /// * If the predicate returns `Ok(None)` the run loop will continue running the core that halted.
    /// * If the predicate returns `Err(e)` the run loop will return `Err(e)`.
    ///
    /// The function will also return on timeout with `Ok(ReturnReason::Timeout)` or if the user presses CTRL + C with `Ok(ReturnReason::Cancelled)`.
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

            poller.start(&mut core)?;

            if core.core_halted()? {
                core.run()?;
            }
        }

        let result = self.do_run_until(shared_session, &mut poller, timeout, &mut predicate);

        // Clean up run loop
        let mut session = shared_session.session_blocking();
        let mut core = session.core(self.core_id)?;
        // Always clean up after RTT but don't overwrite the original result.
        let poller_exit_result = poller.exit(&mut core);
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
        let core_count = shared_session.session_blocking().target().cores.len();
        let mut next_wakeup = vec![start; core_count];

        loop {
            let mut next_poll;

            {
                let mut session = shared_session.session_blocking();

                {
                    let mut core = session.core(self.core_id)?;
                    match self.poll_core(&mut core, true, poller, predicate)? {
                        ControlFlow::Break(reason) => return Ok(reason),
                        ControlFlow::Continue(duration) => next_poll = duration,
                    }
                }

                if self.cancellation_token.is_cancelled() {
                    return Ok(ReturnReason::Cancelled);
                }

                let now = Instant::now();
                for (idx, wakeup) in next_wakeup.iter_mut().enumerate() {
                    if idx == self.core_id {
                        continue;
                    }

                    if now < *wakeup {
                        next_poll = next_poll.min(wakeup.saturating_duration_since(now));
                        continue;
                    }

                    let mut core = match session.core(idx) {
                        Ok(core) => core,
                        Err(Error::CoreDisabled(_)) => {
                            next_wakeup[idx] = Instant::now() + WATCH_POLL_INTERVAL;
                            next_poll = next_poll.min(WATCH_POLL_INTERVAL);
                            continue;
                        }
                        Err(error) => {
                            tracing::debug!(
                                "Skipping core {idx} while the run loop observes it: {error}"
                            );
                            *wakeup = Instant::now() + WATCH_POLL_INTERVAL;
                            next_poll = next_poll.min(WATCH_POLL_INTERVAL);
                            continue;
                        }
                    };

                    match self.poll_core(&mut core, false, poller, predicate) {
                        Ok(ControlFlow::Break(reason)) => return Ok(reason),
                        Ok(ControlFlow::Continue(duration)) => {
                            *wakeup = Instant::now() + duration;
                            next_poll = next_poll.min(duration);
                        }
                        Err(error) => {
                            tracing::debug!(
                                "Skipping core {idx} while the run loop observes it: {error}"
                            );
                            *wakeup = Instant::now() + WATCH_POLL_INTERVAL;
                            next_poll = next_poll.min(WATCH_POLL_INTERVAL);
                        }
                    }

                    if self.cancellation_token.is_cancelled() {
                        return Ok(ReturnReason::Cancelled);
                    }
                }
            }

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

    fn poll_core<F, R>(
        &self,
        core: &mut Core<'_>,
        is_primary: bool,
        poller: &mut impl RunLoopPoller,
        predicate: &mut F,
    ) -> Result<ControlFlow<ReturnReason<R>, Duration>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let mut next_poll = if is_primary {
            DEFAULT_POLL_INTERVAL
        } else {
            WATCH_POLL_INTERVAL
        };

        // Check for halt first. Poll RTT after on the primary core so one last poll after halt
        // flushes messages the core printed before halting, such as a panic message.
        let return_reason = match core.status()? {
            probe_rs::CoreStatus::Halted(reason) => match predicate(reason, core) {
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

        if is_primary {
            let poller_result = poller.poll(core);

            if let Some(reason) = return_reason {
                return reason.map(ControlFlow::Break);
            }
            next_poll = next_poll.min(poller_result?);
        } else if let Some(reason) = return_reason {
            return reason.map(ControlFlow::Break);
        }

        Ok(ControlFlow::Continue(next_poll))
    }
}

pub trait RunLoopPoller {
    fn start(&mut self, core: &mut Core<'_>) -> Result<()>;
    fn poll(&mut self, core: &mut Core<'_>) -> Result<Duration>;
    fn exit(&mut self, core: &mut Core<'_>) -> Result<()>;
}

pub struct NoopPoller;

impl RunLoopPoller for NoopPoller {
    fn start(&mut self, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }

    fn poll(&mut self, _core: &mut Core<'_>) -> Result<Duration> {
        Ok(Duration::from_secs(u64::MAX))
    }

    fn exit(&mut self, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }
}

impl<T> RunLoopPoller for Option<T>
where
    T: RunLoopPoller,
{
    fn start(&mut self, core: &mut Core<'_>) -> Result<()> {
        if let Some(poller) = self {
            poller.start(core)
        } else {
            NoopPoller.start(core)
        }
    }

    fn poll(&mut self, core: &mut Core<'_>) -> Result<Duration> {
        if let Some(poller) = self {
            poller.poll(core)
        } else {
            NoopPoller.poll(core)
        }
    }

    fn exit(&mut self, core: &mut Core<'_>) -> Result<()> {
        if let Some(poller) = self {
            poller.exit(core)
        } else {
            NoopPoller.exit(core)
        }
    }
}
