use anyhow::bail;
use indexmap::IndexMap;
use probe_rs::CoreType;
use probe_rs_rpc_client::SessionInterface;
use tokio::runtime::Handle;

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::Child;
use std::time::Duration;

use super::GdbSessionContext;
use super::target;

const CONNECTION_STRING: &str = "127.0.0.1:1337";

/// Configuration for a single GDB endpoint
pub struct GdbInstanceConfiguration {
    /// The core type that will be sent to GDB
    pub core_type: CoreType,
    /// The list of core indices to expose.
    pub cores: Vec<usize>,
    /// The list of [SocketAddr] addresses to bind to
    pub socket_addrs: Vec<SocketAddr>,
}

impl GdbInstanceConfiguration {
    /// Build a GDB configuration from cached session context. All cores are included.
    pub fn from_context(
        context: &GdbSessionContext,
        connection_string: Option<impl AsRef<str>>,
    ) -> Vec<Self> {
        let connection_string = connection_string
            .as_ref()
            .map(|s| s.as_ref())
            .unwrap_or(CONNECTION_STRING);

        let addrs: Vec<SocketAddr> = connection_string.to_socket_addrs().unwrap().collect();

        let mut groups = IndexMap::new();
        for core in &context.cores {
            groups
                .entry(core.core_type)
                .or_insert_with(Vec::new)
                .push(core.index);
        }

        groups
            .into_iter()
            .enumerate()
            .map(|(i, (core_type, cores))| GdbInstanceConfiguration {
                core_type,
                cores,
                socket_addrs: adjust_addrs(&addrs, i),
            })
            .collect()
    }
}

/// Run a new GDB session over RPC.
pub fn run<'a>(
    session: SessionInterface,
    handle: Handle,
    context: GdbSessionContext,
    instances: impl Iterator<Item = &'a GdbInstanceConfiguration>,
    mut gdb_process: Option<Child>,
) -> anyhow::Result<()> {
    let mut targets = instances
        .map(|instance| {
            target::RuntimeTarget::new(
                session.clone(),
                handle.clone(),
                &context,
                instance.cores.to_vec(),
                &instance.socket_addrs[..],
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    if targets.is_empty() {
        return Ok(());
    }

    loop {
        if let Some(gdb_process) = &mut gdb_process
            && let Some(exit_status) = gdb_process.try_wait()?
        {
            if !exit_status.success() {
                bail!("Gdb failed with {exit_status}");
            }
            return Ok(());
        }

        let mut wait_time = Duration::MAX;

        for target in targets.iter_mut() {
            wait_time = wait_time.min(target.process()?);
        }

        std::thread::sleep(wait_time);
    }
}

fn adjust_addrs(addrs: &[SocketAddr], offset: usize) -> Vec<SocketAddr> {
    addrs
        .iter()
        .map(|addr| {
            let mut new_addr = *addr;
            new_addr.set_port(new_addr.port() + offset as u16);
            new_addr
        })
        .collect()
}
