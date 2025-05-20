use std::time::Duration;

use super::RuntimeTarget;

use gdbstub::target::ext::monitor_cmd::ConsoleOutput;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;
use gdbstub::target::ext::monitor_cmd::outputln;

const HELP_TEXT: &str = r#"Supported Commands:

    info - print session information
    reset - reset target
    reset halt - reset target and halt afterwards
"#;

impl MonitorCmd for RuntimeTarget<'_> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        pollster::block_on(async move {
            match cmd {
                b"info" => outputln!(
                    out,
                    "Target info:\n\n{:#?}",
                    self.session.lock().await.target()
                ),
                b"reset" => {
                    outputln!(out, "Resetting target");
                    match self.session.lock().await.core(0).await?.reset().await {
                        Ok(_) => outputln!(out, "Done"),
                        Err(e) => outputln!(out, "Error while resetting target:\n\t{}", e),
                    }
                }
                b"reset halt" => {
                    let timeout = Duration::from_secs(1);
                    outputln!(out, "Resetting and halting target");
                    match self
                        .session
                        .lock()
                        .await
                        .core(0)
                        .await?
                        .reset_and_halt(timeout)
                        .await
                    {
                        Ok(_) => outputln!(out, "Target halted"),
                        Err(e) => outputln!(out, "Error while halting target:\n\t{}", e),
                    }
                }
                _ => outputln!(out, "{}", HELP_TEXT),
            }

            Ok(())
        })
    }
}
