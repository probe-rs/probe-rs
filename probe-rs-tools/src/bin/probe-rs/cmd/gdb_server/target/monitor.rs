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

impl MonitorCmd for RuntimeTarget {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        match cmd {
            b"info" => {
                outputln!(out, "Target: {}", self.target_name);
                for core in &self.cores {
                    outputln!(
                        out,
                        "  core {}: {} ({:?})",
                        core.index,
                        core.name,
                        core.core_type
                    );
                }
            }
            b"reset" => {
                outputln!(out, "Resetting target");
                match self.block_on(self.session.core(0).reset()) {
                    Ok(_) => outputln!(out, "Done"),
                    Err(e) => outputln!(out, "Error while resetting target:\n\t{}", e),
                }
            }
            b"reset halt" => {
                let timeout = Duration::from_secs(1);
                outputln!(out, "Resetting and halting target");
                match self.block_on(self.session.core(0).reset_and_halt(timeout)) {
                    Ok(_) => outputln!(out, "Target halted"),
                    Err(e) => outputln!(out, "Error while halting target:\n\t{}", e),
                }
            }
            _ => outputln!(out, "{}", HELP_TEXT),
        }

        Ok(())
    }
}
