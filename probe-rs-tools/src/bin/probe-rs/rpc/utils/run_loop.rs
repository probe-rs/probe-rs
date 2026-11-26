use tokio_util::sync::CancellationToken;

use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{Core, CoreType, Error, HaltReason, MemoryInterface, VectorCatchCondition};

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

/// What the run loop does with a core after a handler saw its halt or a reply.
pub enum HaltAction<R> {
    /// The run loop returns.
    Return(R),
    /// The core runs again.
    Resume,
    /// The core stays halted until the reply to a pending call comes.
    Wait,
}

/// Decides what a halt means. A handler can answer a halt later, with the session unlocked.
pub trait HaltHandler {
    type Output;
    type Reply;

    /// A core halted.
    fn on_halt(
        &mut self,
        reason: HaltReason,
        core: &mut Core<'_>,
    ) -> Result<HaltAction<Self::Output>>;

    /// Returns the core that a reply belongs to, or `None` if the reply is stale.
    fn reply_core(&self, _reply: &Self::Reply) -> Option<usize> {
        None
    }

    /// The reply to the pending call of `core` came.
    fn on_reply(
        &mut self,
        _reply: Self::Reply,
        _core: &mut Core<'_>,
    ) -> Result<HaltAction<Self::Output>> {
        unreachable!("`reply_core` selects no core, so no reply gets here")
    }

    /// Lets the handler continue the pending call of `core`, for example to send it when no other
    /// call is in flight.
    fn continue_pending(&mut self, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }

    /// The pending call of a core ended without a reply, because the core does not wait for it any
    /// more. For example, a watchdog reset the core.
    fn on_interrupted(&mut self, _core_id: usize) {}

    /// The run loop ends while the call of `core` is pending. The run loop runs the core after this.
    fn abort_pending(&mut self, _core: &mut Core<'_>) -> Result<()> {
        Ok(())
    }

    /// Waits for at most `timeout`. Returns early with a reply, if it comes.
    fn wait(&mut self, timeout: Duration) -> Option<Self::Reply> {
        thread::sleep(timeout);
        None
    }
}

/// A halt that the run loop saw.
#[derive(Clone, Copy)]
struct Halt {
    reason: HaltReason,
    /// When the run loop saw the halt.
    at: Instant,
}

/// The halt of a core with a pending call.
#[derive(Clone, Copy)]
struct PendingHalt {
    reason: HaltReason,
    pc: u64,
    /// When the run loop saw the halt.
    since: Instant,
}

impl PendingHalt {
    fn read(halt: Halt, core: &mut Core<'_>) -> Result<Self> {
        Ok(Self {
            reason: halt.reason,
            pc: core.read_core_reg(core.program_counter())?,
            since: halt.at,
        })
    }

    /// Returns `true` if the core still waits at the same call.
    fn is_current(&self, status: probe_rs::CoreStatus, core: &mut Core<'_>) -> Result<bool> {
        Ok(status == probe_rs::CoreStatus::Halted(self.reason)
            && self.pc == core.read_core_reg::<u64>(core.program_counter())?)
    }
}

/// The result of checking a core.
enum CoreCheck<R> {
    /// The run loop continues.
    Continue,
    /// The run loop continues. The handler answered a halt, and the core runs again after it was
    /// halted for this time.
    Resumed(Duration),
    /// The run loop returns this, if the core is the primary one.
    Stop(Result<ReturnReason<R>>),
    /// A pending call failed. The run loop returns this on every core.
    PendingFailed(anyhow::Error),
}

/// Default interval between status polls of the primary core when RTT does not request a faster poll.
pub(crate) const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Interval between status polls of other cores. Longer than the primary interval to limit probe traffic.
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

impl RunLoop {
    /// Starts `poller`. Call this before resetting the target: the reset can start cores that
    /// probe-rs does not halt, and they may write what the poller clears.
    pub fn start_poller(
        &self,
        shared_session: &SessionState<'_>,
        poller: &mut impl RunLoopPoller,
    ) -> Result<()> {
        let mut session = shared_session.session_blocking();
        let mut core = session.core(self.core_id)?;
        match poller.memory_port(&core) {
            Some(index) => {
                drop(core);
                poller.start(&mut session.memory_access_port(index)?)
            }
            None => poller.start(&mut core),
        }
    }

    /// Attaches to RTT and runs the primary core until it, or another enabled core, halts.
    ///
    /// Vector catch, the initial resume, and the poller run only on [`Self::core_id`]. Other cores
    /// are observed with `status()` only. A disabled hart is skipped and retried later. An
    /// unexpected halt or semihosting result on any observed core uses the same predicate as the
    /// primary core. A lock-up of another core does not end the run loop.
    ///
    /// Upon halt the predicate is invoked with the halt reason:
    /// * If the predicate returns `Ok(Some(r))` the run loop returns `Ok(ReturnReason::Predicate(r))`.
    /// * If the predicate returns `Ok(None)` the run loop will continue running the core that halted.
    /// * If the predicate returns `Err(e)` the run loop will return `Err(e)`.
    ///
    /// The function will also return on timeout with `Ok(ReturnReason::Timeout)` or if the user presses CTRL + C with `Ok(ReturnReason::Cancelled)`.
    pub fn run_with_handler<H: HaltHandler>(
        &mut self,
        shared_session: &SessionState<'_>,
        vector_catch: VectorCatchConfig,
        mut poller: impl RunLoopPoller,
        timeout: Option<Duration>,
        mut handler: H,
    ) -> Result<ReturnReason<H::Output>> {
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

            if core.core_halted()? {
                core.run()?;
            }
        }

        let core_count = shared_session.session_blocking().target().cores.len();
        let mut pending = vec![None; core_count];
        let result = self.do_run_until(
            shared_session,
            &mut poller,
            timeout,
            &mut handler,
            &mut pending,
        );

        // Clean up run loop
        let mut session = shared_session.session_blocking();
        for (core_id, halt) in pending
            .iter()
            .enumerate()
            .filter_map(|(id, halt)| Some((id, (*halt)?)))
        {
            let aborted = session
                .core(core_id)
                .map_err(Into::into)
                .and_then(|mut core| {
                    // A core that left the call, for example after a reset by another request,
                    // must not get the result, and must not run if someone else halted it.
                    let status = core.status()?;
                    if !halt.is_current(status, &mut core)? {
                        handler.on_interrupted(core_id);
                        return Ok(());
                    }
                    handler.abort_pending(&mut core)?;
                    core.run()?;
                    anyhow::Ok(())
                });
            if let Err(error) = aborted {
                tracing::warn!("Failed to end the pending call of core {core_id}: {error:?}");
            }
        }

        // Always clean up after RTT but don't overwrite the original result.
        let mut core = session.core(self.core_id)?;
        let poller_exit_result = match poller.memory_port(&core) {
            Some(index) => {
                drop(core);
                session
                    .memory_access_port(index)
                    .map_err(Into::into)
                    .and_then(|mut port| poller.exit(&mut port))
            }
            None => poller.exit(&mut core),
        };
        if result.is_ok() {
            // If the result is Ok, we return the potential error during cleanup.
            poller_exit_result?;
        }

        result
    }

    fn do_run_until<H: HaltHandler>(
        &mut self,
        shared_session: &SessionState<'_>,
        poller: &mut impl RunLoopPoller,
        timeout: Option<Duration>,
        handler: &mut H,
        pending: &mut [Option<PendingHalt>],
    ) -> Result<ReturnReason<H::Output>> {
        let start = Instant::now();
        let mut next_wakeup = vec![start; pending.len()];
        let mut reply = None;
        let mut deadline = timeout.map(|timeout| start + timeout);
        // The end of the time that the deadline already moved for.
        let mut paused_until = start;
        // When each core last made a semihosting call that the handler answered.
        let mut last_call = vec![None; pending.len()];

        loop {
            let mut next_poll;
            // The time that cores were halted for a call that the handler answered at once.
            let mut paused = Duration::ZERO;
            let waiting_since = earliest_wait(pending);

            {
                let mut session = shared_session.session_blocking();

                {
                    next_poll = DEFAULT_POLL_INTERVAL;
                    let mut core = session.core(self.core_id)?;
                    let memory_port = poller.memory_port(&core);
                    let core_reply = take_reply_of(&mut reply, handler, self.core_id);
                    let return_reason = match self.check_core(
                        &mut core,
                        &mut next_poll,
                        handler,
                        &mut pending[self.core_id],
                        core_reply,
                    )? {
                        CoreCheck::Continue => None,
                        CoreCheck::Resumed(halted) => {
                            last_call[self.core_id] = Some(Instant::now());
                            paused += halted;
                            None
                        }
                        CoreCheck::Stop(reason) => Some(reason),
                        CoreCheck::PendingFailed(error) => Some(Err(error)),
                    };

                    // Poll RTT after the halt check, so one last poll after a halt flushes
                    // messages the core printed before halting, such as a panic message.
                    let poller_result = match memory_port {
                        Some(index) => {
                            drop(core);
                            session
                                .memory_access_port(index)
                                .map_err(Into::into)
                                .and_then(|mut port| poller.poll(&mut port))
                        }
                        None => poller.poll(&mut core),
                    };

                    if let Some(reason) = return_reason {
                        return reason;
                    }
                    // The poll fails once the client stops listening.
                    if self.cancellation_token.is_cancelled() {
                        return Ok(ReturnReason::Cancelled);
                    }
                    next_poll = next_poll.min(poller_result?);
                }

                let now = Instant::now();
                for (idx, wakeup) in next_wakeup.iter_mut().enumerate() {
                    if idx == self.core_id {
                        continue;
                    }

                    // A core with a pending call needs no status poll, so this adds no traffic.
                    if now < *wakeup && pending[idx].is_none() {
                        next_poll = next_poll.min(wakeup.saturating_duration_since(now));
                        continue;
                    }

                    let was_pending = pending[idx].is_some();
                    let mut core = match session.core(idx) {
                        Ok(core) => core,
                        Err(Error::CoreDisabled(_)) => {
                            if was_pending {
                                // The hart was reset, so it does not wait for the call any more.
                                pending[idx] = None;
                                handler.on_interrupted(idx);
                            }
                            // Retry on the next primary poll so a hart that leaves reset is
                            // configured before it can panic.
                            continue;
                        }
                        // A pending call must not be lost, so an error ends the run loop.
                        Err(error) if was_pending => return Err(error.into()),
                        Err(error) => {
                            tracing::debug!(
                                "Skipping core {idx} while the run loop observes it: {error}"
                            );
                            *wakeup = Instant::now() + WATCH_POLL_INTERVAL;
                            next_poll = next_poll.min(WATCH_POLL_INTERVAL);
                            continue;
                        }
                    };

                    let mut duration = WATCH_POLL_INTERVAL;
                    let core_reply = take_reply_of(&mut reply, handler, idx);
                    match self.check_core(
                        &mut core,
                        &mut duration,
                        handler,
                        &mut pending[idx],
                        core_reply,
                    ) {
                        // A core that is not being run, e.g. one with nothing flashed, may lock up
                        // without affecting the program on the primary core.
                        Ok(CoreCheck::Stop(Ok(ReturnReason::LockedUp))) => {
                            tracing::debug!("Core {idx} is locked up");
                            *wakeup = Instant::now() + WATCH_POLL_INTERVAL;
                            next_poll = next_poll.min(WATCH_POLL_INTERVAL);
                        }
                        Ok(CoreCheck::Stop(Ok(reason))) => return Ok(reason),
                        Ok(CoreCheck::PendingFailed(error)) => return Err(error),
                        Ok(CoreCheck::Stop(Err(error))) | Err(error) if was_pending => {
                            return Err(error);
                        }
                        Ok(check @ (CoreCheck::Continue | CoreCheck::Resumed(_))) => {
                            if let CoreCheck::Resumed(halted) = check {
                                last_call[idx] = Some(Instant::now());
                                paused += halted;
                            }
                            *wakeup = Instant::now() + duration;
                            next_poll = next_poll.min(duration);
                        }
                        Ok(CoreCheck::Stop(Err(error))) | Err(error) => {
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

            // The clock stops while a core is halted for a semihosting call, from the halt to the
            // resume. A slow file system or network must not use up the time of a test, but a test
            // that hangs while it prints still times out. The session is not locked during a wait,
            // so other requests and a cancel still work. A call of any core stops the clock, and
            // overlapping calls count once.
            let now = Instant::now();
            if let Some(since) = waiting_since
                .into_iter()
                .chain(earliest_wait(pending))
                .min()
            {
                paused += now.saturating_duration_since(since.max(paused_until));
                paused_until = now;
            }
            if let Some(deadline) = &mut deadline {
                *deadline += paused;
            }

            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return Ok(ReturnReason::Timeout);
            }

            // Firmware that made a semihosting call often makes the next one soon, so a core is
            // polled again after about as long as its last call is old. This costs a few polls per
            // call, no more than RTT output, which polls with no delay while data comes.
            let now = Instant::now();
            for (idx, last) in last_call.iter_mut().enumerate() {
                let interval = if idx == self.core_id {
                    DEFAULT_POLL_INTERVAL
                } else {
                    WATCH_POLL_INTERVAL
                };
                let Some(delay) = last.and_then(|at| backoff_delay(now - at, interval)) else {
                    *last = None;
                    continue;
                };
                next_poll = next_poll.min(delay);
                if idx != self.core_id {
                    next_wakeup[idx] = next_wakeup[idx].min(now + delay);
                }
            }

            // If the polling frequency is too high, the USB connection to the probe
            // can become unstable. Hence we only poll as little as necessary.
            reply = handler.wait(next_poll);
        }
    }

    /// Checks the core for a reply or a halt, and runs the handler on it.
    fn check_core<H: HaltHandler>(
        &self,
        core: &mut Core<'_>,
        next_poll: &mut Duration,
        handler: &mut H,
        pending: &mut Option<PendingHalt>,
        reply: Option<H::Reply>,
    ) -> Result<CoreCheck<H::Output>> {
        let Some(halt) = *pending else {
            return self.check_status(core.status()?, core, next_poll, handler, pending);
        };

        let Some(reply) = reply else {
            return Ok(match handler.continue_pending(core) {
                Ok(()) => CoreCheck::Continue,
                Err(error) => CoreCheck::PendingFailed(error),
            });
        };

        let status = core.status()?;
        if !halt.is_current(status, core)? {
            // Stale data must not go into firmware that restarted during the call.
            tracing::warn!(
                "Core {} left a semihosting call during the call.",
                core.id()
            );
            *pending = None;
            handler.on_interrupted(core.id());
            return self.check_status(status, core, next_poll, handler, pending);
        }

        // The handler can now send the next call, maybe for a core that this pass already checked.
        *next_poll = Duration::ZERO;
        let action = handler.on_reply(reply, core);
        Ok(action
            .and_then(|action| {
                // The wait for the reply already moved the deadline.
                let halt = Halt {
                    reason: halt.reason,
                    at: Instant::now(),
                };
                self.apply_action(action, halt, core, next_poll, handler, pending)
            })
            .unwrap_or_else(CoreCheck::PendingFailed))
    }

    /// Runs the handler if `status` is a halt.
    fn check_status<H: HaltHandler>(
        &self,
        status: probe_rs::CoreStatus,
        core: &mut Core<'_>,
        next_poll: &mut Duration,
        handler: &mut H,
        pending: &mut Option<PendingHalt>,
    ) -> Result<CoreCheck<H::Output>> {
        match status {
            probe_rs::CoreStatus::Halted(reason) => {
                let halt = Halt {
                    reason,
                    at: Instant::now(),
                };
                match handler.on_halt(reason, core) {
                    Ok(action) => {
                        self.apply_action(action, halt, core, next_poll, handler, pending)
                    }
                    Err(error) => Ok(CoreCheck::Stop(Err(error))),
                }
            }
            probe_rs::CoreStatus::Running
            | probe_rs::CoreStatus::Sleeping
            | probe_rs::CoreStatus::Unknown => {
                // Carry on
                Ok(CoreCheck::Continue)
            }

            probe_rs::CoreStatus::LockedUp => Ok(CoreCheck::Stop(Ok(ReturnReason::LockedUp))),
        }
    }

    fn apply_action<H: HaltHandler>(
        &self,
        action: HaltAction<H::Output>,
        halt: Halt,
        core: &mut Core<'_>,
        next_poll: &mut Duration,
        handler: &mut H,
        pending: &mut Option<PendingHalt>,
    ) -> Result<CoreCheck<H::Output>> {
        match action {
            HaltAction::Return(output) => {
                *pending = None;
                Ok(CoreCheck::Stop(Ok(ReturnReason::Predicate(output))))
            }
            HaltAction::Resume => {
                *pending = None;
                // Re-poll immediately if the core was halted, to speed up reading strings
                // from semihosting. The core is not expected to be halted for other reasons.
                *next_poll = Duration::ZERO;
                core.run()?;
                Ok(CoreCheck::Resumed(halt.at.elapsed()))
            }
            HaltAction::Wait => {
                if pending.is_none() {
                    match PendingHalt::read(halt, core) {
                        Ok(halt) => *pending = Some(halt),
                        Err(error) => {
                            // The handler took the call, but the run loop cannot wait for it.
                            handler.on_interrupted(core.id());
                            return Err(error);
                        }
                    }
                }
                Ok(match handler.continue_pending(core) {
                    Ok(()) => CoreCheck::Continue,
                    Err(error) => CoreCheck::PendingFailed(error),
                })
            }
        }
    }
}

/// Returns when the earliest of the pending calls began.
fn earliest_wait(pending: &[Option<PendingHalt>]) -> Option<Instant> {
    pending.iter().flatten().map(|halt| halt.since).min()
}

/// Returns the delay of the next poll of a core whose last semihosting call is `since` old, or
/// `None` if its normal poll `interval` applies again.
fn backoff_delay(since: Duration, interval: Duration) -> Option<Duration> {
    (since < interval).then(|| since.max(Duration::from_millis(1)))
}

/// Takes `reply` if it belongs to `core_id`. Drops a stale reply.
fn take_reply_of<H: HaltHandler>(
    reply: &mut Option<H::Reply>,
    handler: &H,
    core_id: usize,
) -> Option<H::Reply> {
    match reply.as_ref().map(|reply| handler.reply_core(reply)) {
        Some(Some(id)) if id == core_id => reply.take(),
        Some(None) => {
            *reply = None;
            None
        }
        _ => None,
    }
}

pub trait RunLoopPoller {
    fn start(&mut self, core: &mut impl MemoryInterface) -> Result<()>;

    /// Returns the memory port to poll through instead of the core.
    fn memory_port(&mut self, _core: &Core) -> Option<usize> {
        None
    }

    fn poll(&mut self, core: &mut impl MemoryInterface) -> Result<Duration>;
    fn exit(&mut self, core: &mut impl MemoryInterface) -> Result<()>;
}

pub struct NoopPoller;

impl RunLoopPoller for NoopPoller {
    fn start(&mut self, _core: &mut impl MemoryInterface) -> Result<()> {
        Ok(())
    }

    fn poll(&mut self, _core: &mut impl MemoryInterface) -> Result<Duration> {
        Ok(Duration::from_secs(u64::MAX))
    }

    fn exit(&mut self, _core: &mut impl MemoryInterface) -> Result<()> {
        Ok(())
    }
}

/// Runs two pollers through the memory interface that the first one selects.
impl<A, B> RunLoopPoller for (A, B)
where
    A: RunLoopPoller,
    B: RunLoopPoller,
{
    fn start(&mut self, core: &mut impl MemoryInterface) -> Result<()> {
        self.0.start(core)?;
        self.1.start(core)
    }

    fn memory_port(&mut self, core: &Core) -> Option<usize> {
        self.0.memory_port(core)
    }

    fn poll(&mut self, core: &mut impl MemoryInterface) -> Result<Duration> {
        let next = self.0.poll(core)?;
        Ok(next.min(self.1.poll(core)?))
    }

    fn exit(&mut self, core: &mut impl MemoryInterface) -> Result<()> {
        let first = self.0.exit(core);
        let second = self.1.exit(core);
        first.and(second)
    }
}

impl<T> RunLoopPoller for Option<T>
where
    T: RunLoopPoller,
{
    fn start(&mut self, core: &mut impl MemoryInterface) -> Result<()> {
        if let Some(poller) = self {
            poller.start(core)
        } else {
            NoopPoller.start(core)
        }
    }

    fn memory_port(&mut self, core: &Core) -> Option<usize> {
        self.as_mut()?.memory_port(core)
    }

    fn poll(&mut self, core: &mut impl MemoryInterface) -> Result<Duration> {
        if let Some(poller) = self {
            poller.poll(core)
        } else {
            NoopPoller.poll(core)
        }
    }

    fn exit(&mut self, core: &mut impl MemoryInterface) -> Result<()> {
        if let Some(poller) = self {
            poller.exit(core)
        } else {
            NoopPoller.exit(core)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn the_backoff_grows_with_the_age_of_the_call() {
        let delay = |since_ms| {
            backoff_delay(Duration::from_millis(since_ms), DEFAULT_POLL_INTERVAL)
                .map(|delay| delay.as_millis())
        };

        assert_eq!(delay(0), Some(1));
        assert_eq!(delay(1), Some(1));
        assert_eq!(delay(40), Some(40));
        assert_eq!(delay(99), Some(99));
        // The normal poll interval applies again.
        assert_eq!(delay(100), None);
    }

    #[test]
    fn the_backoff_of_another_core_ends_at_its_own_interval() {
        let since = Duration::from_millis(200);

        assert_eq!(backoff_delay(since, DEFAULT_POLL_INTERVAL), None);
        assert_eq!(backoff_delay(since, WATCH_POLL_INTERVAL), Some(since));
    }
}
