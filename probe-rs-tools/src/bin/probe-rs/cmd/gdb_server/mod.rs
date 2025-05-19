//! GDB server

mod arch;
mod stub;
mod target;

pub(crate) use stub::{GdbInstanceConfiguration, run};
use tokio::sync::Mutex;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use probe_rs::{config::Registry, probe::list::Lister};

use crate::util::common_options::ProbeOptions;

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
    pub async fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) =
            pollster::block_on(self.common.simple_attach(registry, lister))?;

        if self.reset_halt {
            pollster::block_on(async {
                session
                    .core(0)
                    .await?
                    .reset_and_halt(Duration::from_millis(100))
                    .await
            })?;
        }

        let gdb_connection_string = self
            .gdb_connection_string
            .unwrap_or_else(|| "localhost:1337".to_string());

        let instances = crate::cmd::gdb_server::GdbInstanceConfiguration::from_session(
            &session,
            Some(gdb_connection_string),
        );

        for instance in instances.iter() {
            println!(
                "Firing up GDB stub for {:?} cores at {:?}",
                instance.core_type, instance.socket_addrs
            );
        }

        let gdb = if let Some(gdb) = self.gdb {
            tokio::task::spawn_local(async move {
                loop {
                    // Don't exit on ctrl-c as you need to use this key combination
                    // to ask gdb to interrupt execution of the tracee.
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

        let session = Mutex::new(session);

        if let Err(e) = run(&session, instances.iter(), gdb).await {
            eprintln!("During the execution of GDB an error was encountered:");
            eprintln!("{e:?}");
        }

        Ok(())
    }
}
