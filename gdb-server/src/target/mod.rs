mod base;
mod breakpoints;
mod desc;
mod monitor;
mod resume;
mod thread;
mod traits;
mod utils;

use super::arch::RuntimeArch;
use gdbstub::stub::state_machine::GdbStubStateMachine;
use probe_rs::{BreakpointCause, CoreStatus, Error, HaltReason, Session};

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

use gdbstub::common::Signal;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::{GdbStub, MultiThreadStopReason};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::BreakpointsOps;
use gdbstub::target::ext::memory_map::MemoryMapOps;
use gdbstub::target::ext::monitor_cmd::MonitorCmdOps;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverrideOps;
use gdbstub::target::Target;

pub(crate) use traits::{GdbErrorExt, ProbeRsErrorExt};

use desc::TargetDescription;

/// Actions for resuming a core
#[derive(Debug, Copy, Clone)]
pub(crate) enum ResumeAction {
    /// Don't change the state
    Unchanged,
    /// Resume core
    Resume,
    /// Single step core
    Step,
}

/// The top level gdbstub target for a probe-rs debug session
pub(crate) struct RuntimeTarget<'a> {
    /// The probe-rs session object
    session: &'a Mutex<Session>,
    /// A list of core IDs for this stub
    cores: Vec<usize>,

    /// TCP listener accepting incoming connections
    listener: TcpListener,
    /// The current GDB stub state machine
    gdb: Option<GdbStubStateMachine<'a, RuntimeTarget<'a>, TcpStream>>,
    /// Resume action to be used upon a continue request
    resume_action: (usize, ResumeAction),

    /// Description of target's architecture and registers
    target_desc: TargetDescription,
}

impl<'a> RuntimeTarget<'a> {
    /// Create a new RuntimeTarget and get ready to start processing GDB input
    pub fn new(
        session: &'a Mutex<Session>,
        cores: Vec<usize>,
        addrs: &[SocketAddr],
    ) -> Result<Self, Error> {
        let listener = TcpListener::bind(addrs).into_error()?;
        listener.set_nonblocking(true).into_error()?;

        Ok(Self {
            session,
            cores,
            listener,
            gdb: None,
            resume_action: (0, ResumeAction::Unchanged),
            target_desc: TargetDescription::default(),
        })
    }

    /// Process any pending work for this target
    ///
    /// Returns: Duration to wait before processing this target again
    pub fn process(&mut self) -> Result<Duration, Error> {
        // State 1 - unconnected
        if self.gdb.is_none() {
            // See if we have a connection
            match self.listener.accept() {
                Ok((s, addr)) => {
                    log::info!("New connection from {:#?}", addr);

                    for i in 0..self.cores.len() {
                        let core_id = self.cores[i];
                        // When we first attach to the core, GDB expects us to halt the core, so we do this here when a new client connects.
                        // If the core is already halted, nothing happens if we issue a halt command again, so we always do this no matter of core state.
                        self.session
                            .lock()
                            .unwrap()
                            .core(core_id)?
                            .halt(Duration::from_millis(100))?;

                        self.load_target_desc()?;
                    }

                    // Start the GDB Stub state machine
                    let stub = GdbStub::<RuntimeTarget, _>::new(s);
                    match stub.run_state_machine(self) {
                        Ok(gdbstub) => {
                            self.gdb = Some(gdbstub);
                        }
                        Err(e) => {
                            // Any errors at this state are either IO errors or fatal config errors
                            return Err(anyhow::Error::from(e).into());
                        }
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection yet
                    return Ok(Duration::from_millis(10));
                }
                Err(e) => {
                    // Fatal error
                    return Err(anyhow::Error::from(e).into());
                }
            };
        }

        // Stage 2 - connected
        if self.gdb.is_some() {
            let mut wait_time = Duration::ZERO;
            let gdb = self.gdb.take().unwrap();

            self.gdb = match gdb {
                GdbStubStateMachine::Idle(mut state) => {
                    // Read data if available
                    let next_byte = {
                        let conn = state.borrow_conn();

                        read_if_available(conn)?
                    };

                    if let Some(b) = next_byte {
                        Some(state.incoming_data(self, b).into_error()?)
                    } else {
                        wait_time = Duration::from_millis(10);
                        Some(state.into())
                    }
                }
                GdbStubStateMachine::Running(mut state) => {
                    // Read data if available
                    let next_byte = {
                        let conn = state.borrow_conn();

                        read_if_available(conn)?
                    };

                    if let Some(b) = next_byte {
                        Some(state.incoming_data(self, b).into_error()?)
                    } else {
                        // Check for break
                        let mut stop_reason: Option<MultiThreadStopReason<u64>> = None;
                        {
                            let mut session = self.session.lock().unwrap();

                            for i in &self.cores {
                                let mut core = session.core(*i)?;
                                let status = core.status()?;

                                if let CoreStatus::Halted(reason) = status {
                                    let tid = NonZeroUsize::new(i + 1).unwrap();
                                    stop_reason = Some(match reason {
                                        HaltReason::Breakpoint(BreakpointCause::Hardware)
                                        | HaltReason::Breakpoint(BreakpointCause::Unknown) => {
                                            // Some architectures do not allow us to distinguish between hardware and software breakpoints, so we just treat `Unknown` as hardware breakpoints.
                                            MultiThreadStopReason::HwBreak(tid)
                                        }
                                        HaltReason::Step => MultiThreadStopReason::DoneStep,
                                        _ => MultiThreadStopReason::SignalWithThread {
                                            tid,
                                            signal: Signal::SIGINT,
                                        },
                                    });
                                    break;
                                }
                            }

                            // halt all remaining cores that are still running
                            // GDB expects all or nothing stops
                            if stop_reason.is_some() {
                                for i in &self.cores {
                                    let mut core = session.core(*i)?;
                                    if !core.core_halted()? {
                                        core.halt(Duration::from_millis(100))?;
                                    }
                                }
                            }
                        }

                        if let Some(reason) = stop_reason {
                            Some(state.report_stop(self, reason).into_error()?)
                        } else {
                            wait_time = Duration::from_millis(10);
                            Some(state.into())
                        }
                    }
                }
                GdbStubStateMachine::CtrlCInterrupt(state) => {
                    // Break core, handle interrupt
                    {
                        let mut session = self.session.lock().unwrap();
                        for i in &self.cores {
                            let mut core = session.core(*i)?;

                            core.halt(Duration::from_millis(100))?;
                        }
                    }

                    Some(
                        state
                            .interrupt_handled(
                                self,
                                Some(MultiThreadStopReason::Signal(Signal::SIGINT)),
                            )
                            .into_error()?,
                    )
                }
                GdbStubStateMachine::Disconnected(state) => {
                    log::info!("GDB client disconnected: {:?}", state.get_reason());

                    None
                }
            };

            return Ok(wait_time);
        }

        Ok(Duration::ZERO)
    }
}

impl Target for RuntimeTarget<'_> {
    type Arch = RuntimeArch;
    type Error = Error;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::MultiThread(self)
    }

    fn support_target_description_xml_override(
        &mut self,
    ) -> Option<TargetDescriptionXmlOverrideOps<'_, Self>> {
        Some(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_memory_map(&mut self) -> Option<MemoryMapOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    fn guard_rail_implicit_sw_breakpoints(&self) -> bool {
        true
    }
}

/// Read a byte from a stream if available, otherwise return None
fn read_if_available(conn: &mut TcpStream) -> Result<Option<u8>, Error> {
    match conn.peek() {
        Ok(p) => {
            // Unwrap is safe because peek already showed
            // there's data in the buffer
            match p {
                Some(_) => conn.read().map(Some).into_error(),
                None => Ok(None),
            }
        }
        Err(e) => Err(anyhow::Error::from(e).into()),
    }
}
