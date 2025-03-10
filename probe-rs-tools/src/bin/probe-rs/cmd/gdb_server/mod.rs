//! GDB server

mod arch;
mod stub;
mod target;

pub(crate) use stub::{GdbInstanceConfiguration, run};

use std::time::Duration;

use parking_lot::FairMutex;
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

    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(registry, lister)?;

        if self.reset_halt {
            session
                .core(0)?
                .reset_and_halt(Duration::from_millis(100))?;
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

        let session = FairMutex::new(session);

        if let Err(e) = run(&session, instances.iter()) {
            eprintln!("During the execution of GDB an error was encountered:");
            eprintln!("{e:?}");
        }

        Ok(())
    }
}
