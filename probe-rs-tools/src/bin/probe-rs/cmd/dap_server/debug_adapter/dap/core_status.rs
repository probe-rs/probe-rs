use probe_rs::{BreakpointCause, CoreStatus, HaltReason};

use crate::util::style::ReplAddress;

pub(crate) trait DapStatus {
    fn short_long_status(
        &self,
        program_counter: Option<u64>,
        colorize: bool,
    ) -> (&'static str, String);
}
impl DapStatus for CoreStatus {
    /// Return a tuple with short and long descriptions of the core status for human machine interface.
    ///
    /// The short status matches with the strings implemented by the Microsoft DAP protocol,
    /// e.g. `let (short_status, long status) = CoreStatus::short_long_status(core_status)`
    ///
    /// `colorize` may only be set for text that the client shows in its console.
    fn short_long_status(
        &self,
        program_counter: Option<u64>,
        colorize: bool,
    ) -> (&'static str, String) {
        let at = program_counter
            .map(|pc| {
                format!(
                    " at {}",
                    ReplAddress::new(format!("{pc:#010x}")).colorize(colorize)
                )
            })
            .unwrap_or_default();

        match self {
            CoreStatus::Running => ("continued", "The core is running.".to_string()),
            CoreStatus::Sleeping => ("sleeping", "The core is asleep.".to_string()),
            CoreStatus::LockedUp => (
                "lockedup",
                "The core locked up after an unrecoverable exception.".to_string(),
            ),
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Breakpoint(cause) => {
                    let breakpoint = match cause {
                        BreakpointCause::Hardware => "a hardware breakpoint",
                        BreakpointCause::Software => "a software breakpoint",
                        BreakpointCause::Semihosting(_) => "a semihosting request",
                        BreakpointCause::Unknown => "a breakpoint",
                    };
                    ("breakpoint", format!("Stopped at {breakpoint}{at}."))
                }
                HaltReason::Exception => (
                    "exception",
                    format!("Stopped by an exception, for example an interrupt{at}."),
                ),
                HaltReason::Watchpoint => {
                    ("data breakpoint", format!("Stopped by a watchpoint{at}."))
                }
                HaltReason::Step => ("step", format!("Stopped after one step{at}.")),
                HaltReason::Request => ("pause", format!("Stopped on your request{at}.")),
                HaltReason::External => {
                    ("external", format!("Stopped by an external request{at}."))
                }
                HaltReason::Multiple => (
                    "breakpoint",
                    format!("Stopped for more than one reason{at}."),
                ),
                _other => (
                    "unrecognized",
                    format!("Stopped for an unknown reason{at}."),
                ),
            },
            CoreStatus::Unknown => ("unknown", "The state of the core is unknown.".to_string()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn describes_a_halt_with_and_without_a_program_counter() {
        pretty_assertions::assert_eq!(
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
                .short_long_status(Some(0x2000), false),
            (
                "breakpoint",
                "Stopped at a hardware breakpoint at 0x00002000.".to_string()
            )
        );
        pretty_assertions::assert_eq!(
            CoreStatus::Halted(HaltReason::Request).short_long_status(None, true),
            ("pause", "Stopped on your request.".to_string())
        );
    }

    #[test]
    fn styles_the_program_counter_for_ansi_clients() {
        pretty_assertions::assert_eq!(
            CoreStatus::Halted(HaltReason::Step)
                .short_long_status(Some(0x2000), true)
                .1,
            "Stopped after one step at \u{1b}[33m0x00002000\u{1b}[0m."
        );
    }
}
