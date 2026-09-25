mod base;
mod breakpoints;
mod desc;
mod flash;
mod monitor;
mod resume;
mod thread;
mod traits;
mod utils;

use crate::cmd::gdb_server::arch::RuntimeArch;
use crate::cmd::gdb_server::target::desc::TargetDescription;
use probe_rs::CoreRegisters;
use probe_rs::InstructionSet;
use probe_rs_rpc::chip::MemoryRegion;
use probe_rs_rpc::core_ops::WireBreakpointCause;
use probe_rs_rpc::core_ops::WireCoreStatus;
use probe_rs_rpc::core_ops::WireHaltReason;
use probe_rs_rpc::info::WireFlashSector;
use probe_rs_rpc::{FlashLoader, Key};
use probe_rs_rpc_client::{ClientError, CoreInterface, SessionInterface};
use tokio::runtime::Handle;

use std::future::Future;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::time::Duration;

use gdbstub::common::Signal;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::state_machine::{GdbStubStateMachine, GdbStubStateMachineInner, state};
use gdbstub::stub::{GdbStub, MultiThreadStopReason};
use gdbstub::target::Target;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::BreakpointsOps;
use gdbstub::target::ext::flash::FlashOps;
use gdbstub::target::ext::memory_map::MemoryMapOps;
use gdbstub::target::ext::monitor_cmd::MonitorCmdOps;
use gdbstub::target::ext::target_description_xml_override::TargetDescriptionXmlOverrideOps;

pub(crate) use traits::GdbErrorExt;

use super::GdbSessionContext;

/// Actions for resuming a core
#[derive(Debug, Copy, Clone)]
pub(crate) enum ResumeAction {
    Unchanged,
    Resume,
    Step,
}

/// Cached facts for one core exposed by this stub.
#[derive(Clone)]
pub(crate) struct CoreCache {
    pub index: usize,
    pub name: String,
    pub core_type: probe_rs::CoreType,
    pub registers: &'static CoreRegisters,
    pub instruction_set: InstructionSet,
}

/// The top level gdbstub target for a probe-rs RPC debug session
pub(crate) struct RuntimeTarget {
    session: SessionInterface,
    handle: Handle,
    cores: Vec<CoreCache>,
    target_name: String,
    memory_map: Vec<MemoryRegion>,
    flash_sectors: Vec<WireFlashSector>,

    listener: TcpListener,
    gdb: Option<GdbStubStateMachine<'static, RuntimeTarget, TcpStream>>,
    resume_action: (usize, ResumeAction),

    target_desc: TargetDescription,
    /// Server-side flash loader created for an in-progress GDB `load`.
    flash_loader: Option<Key<FlashLoader>>,
    /// True when GDB already erased sectors via `flash_erase` for this load.
    flash_erased: bool,
    memory_map_xml: Option<String>,
}

impl RuntimeTarget {
    pub fn new(
        session: SessionInterface,
        handle: Handle,
        context: &GdbSessionContext,
        core_indices: Vec<usize>,
        addrs: &[SocketAddr],
    ) -> Result<Self, anyhow::Error> {
        let listener = TcpListener::bind(addrs)?;
        listener.set_nonblocking(true)?;

        let cores = core_indices
            .into_iter()
            .map(|index| {
                context
                    .cores
                    .iter()
                    .find(|c| c.index == index)
                    .cloned()
                    .map(|c| CoreCache {
                        index: c.index,
                        name: c.name,
                        core_type: c.core_type,
                        registers: c.registers,
                        instruction_set: c.instruction_set,
                    })
                    .ok_or_else(|| anyhow::anyhow!("Missing core metadata for core {index}"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            session,
            handle,
            cores,
            target_name: context.target_name.clone(),
            memory_map: context.memory_map.clone(),
            flash_sectors: context.flash_sectors.clone(),
            listener,
            gdb: None,
            resume_action: (0, ResumeAction::Unchanged),
            target_desc: TargetDescription::default(),
            flash_loader: None,
            flash_erased: false,
            memory_map_xml: None,
        })
    }

    pub(crate) fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.handle.block_on(fut)
    }

    pub(crate) fn core(&self, index: usize) -> CoreInterface {
        self.session.core(index)
    }

    pub fn process(&mut self) -> Result<Duration, anyhow::Error> {
        if self.gdb.is_none() {
            let stream = match self.listener.accept() {
                Ok((stream, addr)) => {
                    tracing::info!("New connection from {addr:#?}");
                    stream
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            };

            self.halt_all_cores()?;
            self.load_target_desc()?;
            self.memory_map_xml = Some(self.build_memory_map_xml()?);

            let state_machine = GdbStub::new(stream)
                .run_state_machine(self)
                .map_err(|e| anyhow::anyhow!(e))?;

            self.gdb = Some(state_machine);
        }

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

    fn halt_all_cores(&mut self) -> Result<(), ClientError> {
        let cores = self.cores.iter().map(|core| core.index as u32).collect();
        self.block_on(
            self.session
                .halt_cores(Some(cores), Duration::from_millis(100)),
        )?;
        Ok(())
    }

    fn handle_idle<'a>(
        &mut self,
        mut state: GdbStubStateMachineInner<'a, state::Idle<Self>, Self, TcpStream>,
        wait_time: &mut Duration,
    ) -> Result<Option<GdbStubStateMachine<'a, Self, TcpStream>>, anyhow::Error> {
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

    fn handle_running<'a>(
        &mut self,
        mut state: GdbStubStateMachineInner<'a, state::Running, Self, TcpStream>,
        wait_time: &mut Duration,
    ) -> Result<Option<GdbStubStateMachine<'a, Self, TcpStream>>, anyhow::Error> {
        let next_byte = {
            let conn = state.borrow_conn();
            read_if_available(conn)?
        };

        if let Some(b) = next_byte {
            return Ok(Some(state.incoming_data(self, b)?));
        }

        let cores = self.cores.iter().map(|core| core.index as u32).collect();
        let statuses = self.block_on(self.session.cores_status(Some(cores)))?;

        let mut stop_reason: Option<MultiThreadStopReason<u64>> = None;
        for (index, status) in statuses.statuses {
            let WireCoreStatus::Halted(reason) = status else {
                continue;
            };

            let tid = NonZeroUsize::new(index as usize + 1).unwrap();
            stop_reason = Some(match reason {
                WireHaltReason::Breakpoint(
                    WireBreakpointCause::Hardware | WireBreakpointCause::Unknown,
                ) => MultiThreadStopReason::HwBreak(tid),
                WireHaltReason::Step => MultiThreadStopReason::DoneStep,
                _ => MultiThreadStopReason::SignalWithThread {
                    tid,
                    signal: Signal::SIGINT,
                },
            });
            break;
        }

        let next_state = if let Some(reason) = stop_reason {
            self.halt_all_cores()?;
            state.report_stop(self, reason)?
        } else {
            *wait_time = Duration::from_millis(10);
            state.into()
        };

        Ok(Some(next_state))
    }

    fn handle_ctrl_c<'a>(
        &mut self,
        state: GdbStubStateMachineInner<'a, state::CtrlCInterrupt, Self, TcpStream>,
    ) -> Result<Option<GdbStubStateMachine<'a, Self, TcpStream>>, anyhow::Error> {
        self.halt_all_cores()?;
        let next_state =
            state.interrupt_handled(self, Some(MultiThreadStopReason::Signal(Signal::SIGINT)))?;

        Ok(Some(next_state))
    }
}

impl Target for RuntimeTarget {
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

    fn support_flash_operations(&mut self) -> Option<FlashOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }

    fn guard_rail_implicit_sw_breakpoints(&self) -> bool {
        true
    }
}

fn read_if_available(conn: &mut TcpStream) -> Result<Option<u8>, anyhow::Error> {
    match conn.peek() {
        Ok(p) => match p {
            Some(_) => conn.read().map(Some).map_err(|e| e.into()),
            None => Ok(None),
        },
        Err(e) => Err(anyhow::Error::from(e)),
    }
}
