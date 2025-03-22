use probe_rs::{CoreStatus, HaltReason};

pub(crate) trait DapStatus {
    fn short_long_status(&self, program_counter: Option<u64>) -> (&'static str, String);
}
impl DapStatus for CoreStatus {
    /// Return a tuple with short and long descriptions of the core status for human machine interface / hmi. The short status matches with the strings implemented by the Microsoft DAP protocol, e.g. `let (short_status, long status) = CoreStatus::short_long_status(core_status)`
    fn short_long_status(&self, program_counter: Option<u64>) -> (&'static str, String) {
        match self {
            CoreStatus::Running => ("continued", "Core is running".to_string()),
            CoreStatus::Sleeping => ("sleeping", "Core is in SLEEP mode".to_string()),
            CoreStatus::LockedUp => (
                "lockedup",
                "Core is in LOCKUP status - encountered an unrecoverable exception".to_string(),
            ),
            CoreStatus::Halted(halt_reason) => match halt_reason {
                HaltReason::Breakpoint(cause) => (
                    "breakpoint",
                    format!(
                        "Halted on breakpoint ({:?}) @{}.",
                        cause,
                        if let Some(program_counter) = program_counter {
                            format!("{program_counter:#010x}")
                        } else {
                            "(unspecified location)".to_string()
                        }
                    ),
                ),
                HaltReason::Exception => (
                    "exception",
                    "Core halted due to an exception, e.g. interupt handler".to_string(),
                ),
                HaltReason::Watchpoint => (
                    "data breakpoint",
                    "Core halted due to a watchpoint or data breakpoint".to_string(),
                ),
                HaltReason::Step => (
                    "step",
                    format!(
                        "Halted after a 'step' instruction @{}.",
                        if let Some(program_counter) = program_counter {
                            format!("{program_counter:#010x}")
                        } else {
                            "(unspecified location)".to_string()
                        }
                    ),
                ),
                HaltReason::Request => (
                    "pause",
                    format!(
                        "Core halted due to a user (debugger client) request @{}.",
                        if let Some(program_counter) = program_counter {
                            format!("{program_counter:#010x}")
                        } else {
                            "(unspecified location)".to_string()
                        }
                    ),
                ),
                HaltReason::External => (
                    "external",
                    "Core halted due to an external request".to_string(),
                ),
                _other => (
                    "unrecognized",
                    "Core halted: unrecognized cause".to_string(),
                ),
            },
            CoreStatus::Unknown => ("unknown", "Core status cannot be determined".to_string()),
        }
    }
}
