use probe_rs::architecture::riscv::communication_interface::RiscvError;
use probe_rs::architecture::xtensa::communication_interface::XtensaError;
use tokio_util::sync::CancellationToken;

use std::ops::ControlFlow;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{Core, Error, HaltReason, VectorCatchCondition};

use crate::rpc::SessionState;

pub struct RunLoop {
    pub cancellation_token: CancellationToken,
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
        catch_hardfault: bool,
        catch_reset: bool,
        mut poller: impl RunLoopPoller,
        timeout: Option<Duration>,
        mut predicate: F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let core_count = 1; // shared_session.session_blocking().target().cores.len();

        {
            let mut session = shared_session.session_blocking();
            for idx in 0..core_count {
                let mut core = match session.core(idx) {
                    Ok(core) => core,
                    Err(Error::Xtensa(XtensaError::CoreDisabled)) => continue,
                    Err(Error::Riscv(RiscvError::HartUnavailable)) => continue,
                    Err(e) => return Err(e.into()),
                };
                if catch_hardfault || catch_reset {
                    if !core.core_halted()? {
                        core.halt(Duration::from_millis(100))?;
                    }

                    if catch_hardfault {
                        match core.enable_vector_catch(VectorCatchCondition::HardFault) {
                            Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                            Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                        }
                    }
                    if catch_reset {
                        match core.enable_vector_catch(VectorCatchCondition::CoreReset) {
                            Ok(_) | Err(Error::NotImplemented(_)) => {} // Don't output an error if vector_catch hasn't been implemented
                            Err(e) => tracing::error!("Failed to enable_vector_catch: {:?}", e),
                        }
                    }
                }
                poller.start(&mut core)?;

                if core.core_halted()? {
                    core.run()?;
                }
            }
        }

        let result = self.do_run_until(shared_session, &mut poller, timeout, &mut predicate);

        // Clean up run loop
        let mut session = shared_session.session_blocking();
        // Always clean up after RTT but don't overwrite the original result.
        for idx in 0..core_count {
            let mut core = match session.core(idx) {
                Ok(core) => core,
                Err(Error::Xtensa(XtensaError::CoreDisabled)) => continue,
                Err(Error::Riscv(RiscvError::HartUnavailable)) => continue,
                Err(e) => return Err(e.into()),
            };
            let poller_exit_result = poller.exit(&mut core);
            if result.is_ok() {
                // If the result is Ok, we return the potential error during cleanup.
                poller_exit_result?;
            }
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
        let core_count = 1; // shared_session.session_blocking().target().cores.len();

        let mut next_wakeup = {
            let now = Instant::now();
            vec![now; core_count]
        };

        loop {
            let now = Instant::now();
            let mut session = shared_session.session_blocking();
            for idx in 0..core_count {
                if now < next_wakeup[idx] {
                    continue;
                }

                let mut core = match session.core(idx) {
                    Ok(core) => core,
                    Err(Error::Xtensa(XtensaError::CoreDisabled)) => continue,
                    Err(Error::Riscv(RiscvError::HartUnavailable)) => continue,
                    Err(e) => return Err(e.into()),
                };
                match self.poll_once(&mut core, poller, predicate)? {
                    ControlFlow::Break(reason) => return Ok(reason),
                    ControlFlow::Continue(next_poll) => {
                        if let Some(timeout) = timeout
                            && start.elapsed() >= timeout
                        {
                            return Ok(ReturnReason::Timeout);
                        }

                        next_wakeup[idx] = Instant::now() + next_poll;
                    }
                }
            }

            // If the polling frequency is too high, the USB connection to the probe
            // can become unstable. Hence we only poll as little as necessary.
            thread::sleep(*next_wakeup.iter().min().unwrap() - Instant::now());
        }
    }

    fn poll_once<F, R>(
        &self,
        core: &mut Core<'_>,
        poller: &mut impl RunLoopPoller,
        predicate: &mut F,
    ) -> Result<ControlFlow<ReturnReason<R>, Duration>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        let mut next_poll = Duration::from_millis(100);

        // check for halt first, poll rtt after.
        // this is important so we do one last poll after halt, so we flush all messages
        // the core printed before halting, such as a panic message.
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

        let poller_result = poller.poll(core);

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
