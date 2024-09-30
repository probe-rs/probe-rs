use std::{io::Write as _, num::NonZeroU32};

use crate::cmd::run::{OutputStream, RunLoop, RunMode};
use anyhow::anyhow;
use probe_rs::{semihosting::SemihostingCommand, BreakpointCause, Core, HaltReason, Session};

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
    fn run(&self, mut session: Session, mut run_loop: RunLoop) -> anyhow::Result<()> {
        let mut core = session.core(run_loop.core_id)?;

        let halt_handler = |halt_reason: HaltReason, core: &mut Core| {
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
                SemihostingCommand::Open(request) => {
                    let path = request.path(core)?;
                    if path == ":tt" {
                        request.respond_with_handle(core, NonZeroU32::new(1).unwrap())?;
                    } else {
                        tracing::warn!(
                            "Target wanted to open file {path}, but probe-rs does not support this operation yet. Continuing..."
                        );
                    }
                    Ok(None) // Continue running
                }
                SemihostingCommand::Close(request) => {
                    let handle = request.file_handle(core)?;
                    if handle == 1 {
                        request.success(core)?;
                    } else {
                        tracing::warn!(
                            "Target wanted to close file handle {handle}, but probe-rs does not support this operation yet. Continuing..."
                        );
                    }
                    Ok(None) // Continue running
                }
                SemihostingCommand::Write(request) => {
                    if request.file_handle() == 1 {
                        std::io::stdout().write_all(&request.read(core)?).unwrap();

                        request.write_status(core, 0)?;
                    } else {
                        tracing::warn!(
                            "Target wanted to write to file handle {}, but probe-rs does not support this operation yet. Continuing...",
                            request.file_handle()
                        );
                    }
                    Ok(None) // Continue running
                }
                SemihostingCommand::WriteConsole(request) => {
                    std::io::stdout()
                        .write_all(request.read(core)?.as_bytes())
                        .unwrap();
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
