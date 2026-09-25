use probe_rs::{BreakpointCause, CoreStatus, HaltReason};

use crate::cmd::dap_server::backend::rpc::RpcBackend;
use crate::util::style::format_location;

pub(crate) trait DapStatus {
    async fn short_long_status(
        &self,
        backend: &RpcBackend,
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
    /// The source location of `program_counter` is resolved through `backend`, and is left out
    /// of the description when the address has no debug information.
    ///
    /// `colorize` may only be set for text that the client shows in its console.
    async fn short_long_status(
        &self,
        backend: &RpcBackend,
        program_counter: Option<u64>,
        colorize: bool,
    ) -> (&'static str, String) {
        let mut at = String::new();
        if let Some(pc) = program_counter {
            let source_location = backend.source_location(pc).await;
            at = format!(
                " at {}",
                format_location(pc, source_location.as_ref(), colorize)
            );
        }

        describe(self, &at)
    }
}

/// `at` names where the core stopped, and is empty when the location is unknown.
fn describe(status: &CoreStatus, at: &str) -> (&'static str, String) {
    match status {
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
            HaltReason::Watchpoint => ("data breakpoint", format!("Stopped by a watchpoint{at}.")),
            HaltReason::Step => ("step", format!("Stopped after one step{at}.")),
            HaltReason::Request => ("pause", format!("Stopped on your request{at}.")),
            HaltReason::External => ("external", format!("Stopped by an external request{at}.")),
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn describes_a_halt_with_and_without_a_location() {
        pretty_assertions::assert_eq!(
            describe(
                &CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware)),
                " at 0x00002000 (/src/main.rs:42:7)"
            ),
            (
                "breakpoint",
                "Stopped at a hardware breakpoint at 0x00002000 (/src/main.rs:42:7).".to_string()
            )
        );
        pretty_assertions::assert_eq!(
            describe(&CoreStatus::Halted(HaltReason::Request), ""),
            ("pause", "Stopped on your request.".to_string())
        );
    }
}
