mod base;
mod breakpoints;
mod desc;
mod monitor;
mod resume;
mod thread;
mod traits;
mod utils;

use super::arch::RuntimeArch;
use crate::{BreakpointCause, CoreStatus, Error, HaltReason, Session};
use gdbstub::stub::state_machine::{state, GdbStubStateMachine, GdbStubStateMachineInner};
use parking_lot::FairMutex;

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
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

pub(crate) use traits::GdbErrorExt;

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
    session: &'a FairMutex<Session>,
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
        session: &'a FairMutex<Session>,
        cores: Vec<usize>,
        addrs: &[SocketAddr],
    ) -> Result<Self, anyhow::Error> {
        let listener = TcpListener::bind(addrs)?;
        listener.set_nonblocking(true)?;

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
    pub fn process(&mut self) -> Result<Duration, anyhow::Error> {
        // State 1 - unconnected
        if self.gdb.is_none() {
            // See if we have a connection
            let stream = match self.listener.accept() {
                Ok((stream, addr)) => {
                    tracing::info!("New connection from {addr:#?}");
                    stream
                }
                // No connection yet
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(Duration::from_millis(10));
                }
                // Fatal error
                Err(e) => return Err(e.into()),
            };

            // When we first attach to the core, GDB expects us to halt the core,
            // so we do this here when a new client connects.
            self.halt_all_cores()?;
            self.load_target_desc()?;

            // Start the GDB Stub state machine
            // Any errors at this state are either IO errors or fatal config errors
            let state_machine = GdbStub::new(stream)
                .run_state_machine(self)
                .map_err(|e| anyhow::anyhow!(e))?;

            self.gdb = Some(state_machine);
        }

        // Stage 2 - connected
        let Some(gdb) = self.gdb.take() else {
            return Ok(Duration::ZERO);
        };

        let mut wait_time = Duration::ZERO;

        self.gdb = match gdb {
            GdbStubStateMachine::Idle(state) => self.handle_idle(state, &mut wait_time)?,
            GdbStubStateMachine::Running(state) => self.handle_running(state, &mut wait_time)?,
            GdbStubStateMachine::CtrlCInterrupt(state) => self.handle_ctrl_c(state)?,
            GdbStubStateMachine::Disconnected(state) => {
                tracing::info!("GDB client disconnected: {:?}", state.get_reason());

                None
            }
        };

        Ok(wait_time)
    }

    fn halt_all_cores(&mut self) -> Result<(), Error> {
        let mut session = self.session.lock();

        for i in &self.cores {
            let mut core = session.core(*i)?;
            if !core.core_halted()? {
                core.halt(Duration::from_millis(100))?;
            }
        }

        Ok(())
    }

    fn handle_idle<'b>(
        &mut self,
        mut state: GdbStubStateMachineInner<'b, state::Idle<Self>, Self, TcpStream>,
        wait_time: &mut Duration,
    ) -> Result<Option<GdbStubStateMachine<'b, Self, TcpStream>>, anyhow::Error> {
        let next_byte = {
            let conn = state.borrow_conn();

            read_if_available(conn)?
        };

        let next_state = if let Some(b) = next_byte {
            state.incoming_data(self, b)?
        } else {
            *wait_time = Duration::from_millis(10);
            state.into()
        };

        Ok(Some(next_state))
    }

    fn handle_running<'b>(
        &mut self,
        mut state: GdbStubStateMachineInner<'b, state::Running, Self, TcpStream>,
        wait_time: &mut Duration,
    ) -> Result<Option<GdbStubStateMachine<'b, Self, TcpStream>>, anyhow::Error> {
        let next_byte = {
            let conn = state.borrow_conn();

            read_if_available(conn)?
        };

        if let Some(b) = next_byte {
            return Ok(Some(state.incoming_data(self, b)?));
        }

        // Check for break
        let mut stop_reason: Option<MultiThreadStopReason<u64>> = None;
        {
            let mut session = self.session.lock();

            for i in &self.cores {
                let mut core = session.core(*i)?;
                let CoreStatus::Halted(reason) = core.status()? else {
                    continue;
                };

                let tid = NonZeroUsize::new(i + 1).unwrap();
                stop_reason = Some(match reason {
                    HaltReason::Breakpoint(BreakpointCause::Hardware)
                    | HaltReason::Breakpoint(BreakpointCause::Unknown) => {
                        // Some architectures do not allow us to distinguish between
                        // hardware and software breakpoints, so we just treat `Unknown`
                        // as hardware breakpoints.
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

        let next_state = if let Some(reason) = stop_reason {
            // Halt all remaining cores that are still running.
            // GDB expects all or nothing stops.
            self.halt_all_cores()?;
            state.report_stop(self, reason)?
        } else {
            *wait_time = Duration::from_millis(10);
            state.into()
        };

        Ok(Some(next_state))
    }

    fn handle_ctrl_c<'b>(
        &mut self,
        state: GdbStubStateMachineInner<'b, state::CtrlCInterrupt, Self, TcpStream>,
    ) -> Result<Option<GdbStubStateMachine<'b, Self, TcpStream>>, anyhow::Error> {
        self.halt_all_cores()?;
        let next_state =
            state.interrupt_handled(self, Some(MultiThreadStopReason::Signal(Signal::SIGINT)))?;

        Ok(Some(next_state))
    }
}

impl Target for RuntimeTarget<'_> {
    type Arch = RuntimeArch;
    type Error = anyhow::Error;

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
fn read_if_available(conn: &mut TcpStream) -> Result<Option<u8>, anyhow::Error> {
    match conn.peek() {
        Ok(p) => {
            // Unwrap is safe because peek already showed
            // there's data in the buffer
            match p {
                Some(_) => conn.read().map(Some).map_err(|e| e.into()),
                None => Ok(None),
            }
        }
        Err(e) => Err(anyhow::Error::from(e)),
    }
}
