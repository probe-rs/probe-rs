use crate::cmd::run::{OutputStream, RunLoop, RunMode};
use anyhow::anyhow;
use probe_rs::{BreakpointCause, Core, HaltReason, SemihostingCommand, Session};

/// Options only used in normal run mode
#[derive(Debug, clap::Parser, Clone)]
pub struct NormalRunOptions {
    /// Enable reset vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_reset: bool,
    /// Enable hardfault vector catch if its supported on the target.
    #[clap(long, help_heading = "RUN OPTIONS")]
    pub catch_hardfault: bool,
}

/// Normal run mode (non-test)
pub struct NormalRunMode {
    run_options: NormalRunOptions,
}

impl NormalRunMode {
    pub fn new(run_options: NormalRunOptions) -> Box<Self> {
        Box::new(NormalRunMode { run_options })
    }
}
impl RunMode for NormalRunMode {
    fn run(&self, mut session: Session, run_loop: RunLoop) -> anyhow::Result<()> {
        let mut core = session.core(run_loop.core_id)?;

        let halt_handler = |halt_reason: HaltReason, _core: &mut Core| {
            let HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd)) = halt_reason else {
                anyhow::bail!("CPU halted unexpectedly.");
            };

            match cmd {
                SemihostingCommand::ExitSuccess => Ok(Some(())), // Exit the run loop
                SemihostingCommand::ExitError(details) => {
                    Err(anyhow!("Semihosting indicated exit with {details}"))
                }
                SemihostingCommand::Unknown(details) => {
                    tracing::warn!(
                        "Target wanted to run semihosting operation {:#x} with parameter {:#x},\
                             but probe-rs does not support this operation yet. Continuing...",
                        details.operation,
                        details.parameter
                    );
                    Ok(None) // Continue running
                }
                SemihostingCommand::GetCommandLine(_) => {
                    tracing::warn!("Target wanted to run semihosting operation SYS_GET_CMDLINE, but probe-rs does not support this operation yet. Continuing...");
                    Ok(None) // Continue running
                }
            }
        };

        run_loop.run_until(
            &mut core,
            self.run_options.catch_hardfault,
            self.run_options.catch_reset,
            OutputStream::Stdout,
            None,
            halt_handler,
        )?;
        Ok(())
    }
}
