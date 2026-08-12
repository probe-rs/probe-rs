//! GDB stub driven by a probe-rs RPC [`SessionInterface`].
//!
//! The TCP listener and gdbstub state machine run in the client process. Probe
//! access goes through RPC, so the same session can be shared with other RPC
//! clients (for example an RTT UI) without a local `FairMutex<Session>`.

mod arch;
mod stub;
mod target;

pub(crate) use stub::{GdbInstanceConfiguration, run};

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use probe_rs::CoreRegisters;
use probe_rs::InstructionSet;
use probe_rs::config::Registry;
use probe_rs_rpc::chip::MemoryRegion;
use probe_rs_rpc::info::WireFlashSector;
use probe_rs_rpc_client::{RpcClient, SessionInterface};
use tokio::runtime::Handle;

use crate::rpc::functions::core_ops::convert::{from_wire_core_type, from_wire_instruction_set};
use crate::util::{cli, common_options::ProbeOptions};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(
        long,
        help = "Use this flag to override the default GDB connection string (localhost:1337)."
    )]
    gdb_connection_string: Option<String>,

    #[clap(
        name = "reset-halt",
        long = "reset-halt",
        help = "Use this flag to reset and halt (instead of just a halt) the attached core after attaching to the target."
    )]
    reset_halt: bool,

    #[clap(long, help = "Spawn gdb after starting the gdbserver.")]
    gdb: Option<String>,

    /// The path to the ELF file to debug.
    ///
    /// This only needs to be specified when using `--gdb`.
    #[clap(index = 1)]
    path: Option<PathBuf>,

    #[clap(name = "GDB ARGS", index = 2, help = "Arguments to pass to gdb.")]
    gdb_args: Vec<String>,

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let mut registry = Registry::from_builtin_families();
        if let Some(path) = &self.common.chip_description_path
            && let Ok(yaml) = std::fs::read_to_string(path)
        {
            _ = registry.add_target_family_from_yaml(&yaml);
        }

        let session = cli::attach_probe(&client, self.common, None, false).await?;

        if self.reset_halt {
            session
                .core(0)
                .reset_and_halt(Duration::from_millis(500))
                .await?;
        }

        let gdb_connection_string = self
            .gdb_connection_string
            .unwrap_or_else(|| "localhost:1337".to_string());

        let context = GdbSessionContext::from_session(&session, &registry).await?;
        let instances =
            GdbInstanceConfiguration::from_context(&context, Some(gdb_connection_string));

        for instance in instances.iter() {
            println!(
                "Firing up GDB stub for {:?} cores at {:?}",
                instance.core_type, instance.socket_addrs
            );
        }

        let gdb = if let Some(gdb) = self.gdb {
            tokio::spawn(async move {
                loop {
                    // Don't exit on ctrl-c as you need to use this key combination
                    // to ask gdb to interrupt execution of the trace.
                    tokio::signal::ctrl_c().await.unwrap();
                }
            });

            let mut cmd = Command::new(gdb);
            cmd.args([
                "-ex",
                &format!("target remote {}", instances[0].socket_addrs[0]),
            ]);
            if let Some(path) = self.path {
                cmd.arg("--symbols").arg(path);
            }
            cmd.args(self.gdb_args);
            eprintln!("Spawning {cmd:?}");
            Some(cmd.spawn()?)
        } else {
            None
        };

        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = run(session, handle, context, instances.iter(), gdb) {
                eprintln!("During the execution of GDB an error was encountered:");
                eprintln!("{e:?}");
            }
        })
        .await?;

        Ok(())
    }
}

/// Cached target facts gathered once after attach.
pub(crate) struct GdbSessionContext {
    pub target_name: String,
    pub cores: Vec<GdbCoreInfo>,
    /// Memory map from RPC `target/metadata` for GDB memory-map XML.
    pub memory_map: Vec<MemoryRegion>,
    /// Absolute flash sectors from RPC `target/metadata`.
    pub flash_sectors: Vec<WireFlashSector>,
}

#[derive(Clone)]
pub(crate) struct GdbCoreInfo {
    pub index: usize,
    pub name: String,
    pub core_type: probe_rs::CoreType,
    pub registers: &'static CoreRegisters,
    pub instruction_set: InstructionSet,
}

impl GdbSessionContext {
    pub async fn from_session(
        session: &SessionInterface,
        registry: &Registry,
    ) -> anyhow::Result<Self> {
        let metadata = session.target_metadata().await?;
        let target = registry.get_target_by_name(&metadata.target_name).ok();

        let mut cores = Vec::with_capacity(metadata.cores.len());
        for wire_core in &metadata.cores {
            let core_type = from_wire_core_type(wire_core.core_type);
            let meta = session.core(wire_core.index as usize).metadata().await?;
            let registers = CoreRegisters::for_core_type(
                core_type,
                meta.fpu_support,
                meta.floating_point_register_count
                    .map(|count| count as usize),
            );
            let instruction_set = from_wire_instruction_set(meta.instruction_set);

            let name = target
                .as_ref()
                .and_then(|t| t.cores.get(wire_core.index as usize))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("core{}", wire_core.index));

            cores.push(GdbCoreInfo {
                index: wire_core.index as usize,
                name,
                core_type,
                registers,
                instruction_set,
            });
        }

        Ok(Self {
            target_name: metadata.target_name,
            cores,
            memory_map: metadata.memory_map,
            flash_sectors: metadata.flash_sectors,
        })
    }
}
