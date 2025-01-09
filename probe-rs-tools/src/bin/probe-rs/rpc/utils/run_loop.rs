use tokio_util::sync::CancellationToken;

use std::fmt::Write;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use probe_rs::{rtt::Error as RttError, Core, Error, HaltReason, VectorCatchCondition};

use crate::util::rtt::client::RttClient;
use crate::util::rtt::ChannelDataCallbacks;

pub struct RunLoop<'a> {
    pub core_id: usize,
    pub rtt_client: Option<&'a mut RttClient>,
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
}

impl RunLoop<'_> {
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
        core: &mut Core,
        catch_hardfault: bool,
        catch_reset: bool,
        output_stream: &mut dyn Write,
        timeout: Option<Duration>,
        mut predicate: F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
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

        if core.core_halted()? {
            core.run()?;
        }
        let start = Instant::now();

        let result = self.do_run_until(core, output_stream, timeout, start, &mut predicate);

        // Always clean up after RTT but don't overwrite the original result.
        if let Some(ref mut rtt_client) = self.rtt_client {
            let cleanup_result = rtt_client.clean_up(core);

            if result.is_ok() {
                // If the result is Ok, we return the potential error during cleanup.
                cleanup_result?;
            }
        }

        result
    }

    fn do_run_until<F, R>(
        &mut self,
        core: &mut Core,
        output_stream: &mut dyn Write,
        timeout: Option<Duration>,
        start: Instant,
        predicate: &mut F,
    ) -> Result<ReturnReason<R>>
    where
        F: FnMut(HaltReason, &mut Core) -> Result<Option<R>>,
    {
        loop {
            // check for halt first, poll rtt after.
            // this is important so we do one last poll after halt, so we flush all messages
            // the core printed before halting, such as a panic message.
            let mut return_reason = None;
            let mut was_halted = false;
            match core.status()? {
                probe_rs::CoreStatus::Halted(reason) => match predicate(reason, core) {
                    Ok(Some(r)) => return_reason = Some(Ok(ReturnReason::Predicate(r))),
                    Err(e) => return_reason = Some(Err(e)),
                    Ok(None) => {
                        was_halted = true;
                        core.run()?
                    }
                },
                probe_rs::CoreStatus::Running
                | probe_rs::CoreStatus::Sleeping
                | probe_rs::CoreStatus::Unknown => {
                    // Carry on
                }

                probe_rs::CoreStatus::LockedUp => {
                    return Err(anyhow!("The core is locked up."));
                }
            }

            let had_rtt_data = if let Some(ref mut rtt_client) = self.rtt_client {
                poll_rtt(rtt_client, core, output_stream)?
            } else {
                false
            };

            if let Some(reason) = return_reason {
                return reason;
            } else if let Some(timeout) = timeout {
                if start.elapsed() >= timeout {
                    return Ok(ReturnReason::Timeout);
                }
            }
            if self.cancellation_token.is_cancelled() {
                return Ok(ReturnReason::Cancelled);
            }

            // Poll RTT with a frequency of 10 Hz if we do not receive any new data.
            // Once we receive new data, we bump the frequency to 1kHz.
            //
            // We also poll at 1kHz if the core was halted, to speed up reading strings
            // from semihosting. The core is not expected to be halted for other reasons.
            //
            // If the polling frequency is too high, the USB connection to the probe
            // can become unstable. Hence we only pull as little as necessary.
            if had_rtt_data || was_halted {
                thread::sleep(Duration::from_millis(1));
            } else {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Poll RTT and print the received buffer.
fn poll_rtt<S: Write + ?Sized>(
    rtt_client: &mut RttClient,
    core: &mut Core<'_>,
    out_stream: &mut S,
) -> Result<bool, anyhow::Error> {
    struct OutCollector<'a, O: Write + ?Sized> {
        out_stream: &'a mut O,
        had_data: bool,
    }

    impl<O: Write + ?Sized> ChannelDataCallbacks for OutCollector<'_, O> {
        fn on_string_data(&mut self, _channel: usize, data: String) -> Result<(), RttError> {
            if data.is_empty() {
                return Ok(());
            }
            self.had_data = true;
            self.out_stream
                .write_str(&data)
                .map_err(|err| anyhow!(err))?;
            Ok(())
        }
    }

    let mut out = OutCollector {
        out_stream,
        had_data: false,
    };

    rtt_client.poll(core, &mut out)?;

    Ok(out.had_data)
}
