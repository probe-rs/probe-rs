use std::time::Duration;

use super::RuntimeTarget;

use gdbstub::target::ext::monitor_cmd::outputln;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;

const HELP_TEXT: &str = r#"Supported Commands:

    info - print session information
    reset - reset target
    reset halt - reset target and halt afterwards
"#;

impl MonitorCmd for RuntimeTarget<'_> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: gdbstub::target::ext::monitor_cmd::ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        match cmd {
            b"info" => outputln!(out, "Target info:\n\n{:#?}", self.session.lock().target()),
            b"reset" => {
                outputln!(out, "Resetting target");
                match self.session.lock().core(0)?.reset() {
                    Ok(_) => outputln!(out, "Done"),
                    Err(e) => outputln!(out, "Error while resetting target:\n\t{}", e),
                }
            }
            b"reset halt" => {
                let timeout = Duration::from_secs(1);
                outputln!(out, "Resetting and halting target");
                match self.session.lock().core(0)?.reset_and_halt(timeout) {
                    Ok(_) => outputln!(out, "Target halted"),
                    Err(e) => outputln!(out, "Error while halting target:\n\t{}", e),
                }
            }
            _ => outputln!(out, "{}", HELP_TEXT),
        }

        Ok(())
    }
}
