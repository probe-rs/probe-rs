/// The status of the core.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum CoreStatus {
    /// The core is currently running.
    Running,
    /// The core is currently halted. This also specifies the reason as a payload.
    Halted(HaltReason),
    /// This is a Cortex-M specific status, and will not be set or handled by RISCV code.
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

/// When the core halts due to a breakpoint request, some architectures will allow us to distinguish between a software and hardware breakpoint.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum BreakpointCause {
    /// We encountered a hardware breakpoint.
    Hardware,
    /// We encountered a software breakpoint instruction.
    Software,
    /// We were not able to distinguish if this was a hardware or software breakpoint.
    Unknown,
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
/// `VectorCatch` describes which event exactly should trigger a halt.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum VectorCatchCondition {
    /// We encountered a hardfault.
    HardFault,
    /// We encountered a local reset.
    CoreReset,
    /// We encountered any exception.
    All,
}
