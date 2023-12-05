/// The status of the core.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum CoreStatus {
    /// The core is currently running.
    Running,
    /// The core is currently halted. This also specifies the reason as a payload.
    Halted(HaltReason),
    /// This is a Cortex-M specific status, and will not be set or handled by RISC-V code.
    LockedUp,
    /// The core is currently sleeping.
    Sleeping,
    /// The core state is currently unknown. This is always the case when the core is first created.
    Unknown,
}

impl CoreStatus {
    /// Returns `true` if the core is currently halted.
    pub fn is_halted(&self) -> bool {
        matches!(self, CoreStatus::Halted(_))
    }

    /// Returns `true` if the core is currently running.
    pub fn is_running(&self) -> bool {
        self == &Self::Running
    }
}

/// Indicates the operation the target would like the debugger to perform.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum SemihostingCommand {
    /// The target indicates that it completed successfully and no-longer wishes
    /// to run.
    ExitSuccess,
    /// The target indicates that it completed unsuccessfully, with an error
    /// code, and no-longer wishes to run.
    ExitError {
        /// Some architecture-specific or application specific exit code
        code: u64,
    },
    /// The target indicated that it would like to run a semihosting operation which we don't support yet
    Unknown {
        /// The semihosting operation requested
        operation: u32,

        /// The parameter to the semihosting operation
        parameter: u32,
    },
}

/// When the core halts due to a breakpoint request, some architectures will allow us to distinguish between a software and hardware breakpoint.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum BreakpointCause {
    /// We encountered a hardware breakpoint.
    Hardware,
    /// We encountered a software breakpoint instruction.
    Software,
    /// We were not able to distinguish if this was a hardware or software breakpoint.
    Unknown,
    /// The target requested the host perform a semihosting operation.
    ///
    /// The core set up some registers into a well-specified state and then hit
    /// a breakpoint. This indicates the core would like the debug probe to do
    /// some work.
    Semihosting(SemihostingCommand),
}

/// The reason why a core was halted.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum HaltReason {
    /// Multiple reasons for a halt.
    ///
    /// This can happen for example when a single instruction
    /// step ends up on a breakpoint, after which both breakpoint and step / request
    /// are set.
    Multiple,
    /// Core halted due to a breakpoint. The cause is `Unknown` if we cannot distinguish between a hardware and software breakpoint.
    Breakpoint(BreakpointCause),
    /// Core halted due to an exception, e.g. an
    /// an interrupt.
    Exception,
    /// Core halted due to a data watchpoint
    Watchpoint,
    /// Core halted after single step
    Step,
    /// Core halted because of a debugger request
    Request,
    /// External halt request
    External,
    /// Unknown reason for halt.
    ///
    /// This can happen for example when the core is already halted when we connect.
    Unknown,
}

/// When a core hits an exception, we halt the core.
///
/// `VectorCatchCondition` describes which event exactly should trigger a halt.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum VectorCatchCondition {
    /// We encountered a hardfault.
    HardFault,
    /// We encountered a local reset.
    CoreReset,
    /// We encountered a SecureFault.
    SecureFault,
    /// We encountered any exception.
    All,
}
