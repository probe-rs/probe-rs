use super::RuntimeTarget;

use gdbstub::target::ext::monitor_cmd::outputln;
use gdbstub::target::ext::monitor_cmd::MonitorCmd;

const HELP_TEXT: &str = r#"Supported Commands:

    info - print session information
"#;

impl MonitorCmd for RuntimeTarget<'_> {
    fn handle_monitor_cmd(
        &mut self,
        cmd: &[u8],
        mut out: gdbstub::target::ext::monitor_cmd::ConsoleOutput<'_>,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(cmd);

        match cmd.as_ref() {
            "info" => {
                outputln!(
                    out,
                    "Target info:\n\n{:#?}",
                    self.session.lock().unwrap().target()
                );
            }
            _ => {
                outputln!(out, "{}", HELP_TEXT);
            }
        }

        Ok(())
    }
}
