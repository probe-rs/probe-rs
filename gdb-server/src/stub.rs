use anyhow::Result;
use probe_rs::{CoreType, Error, Session};

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;

use itertools::Itertools;

use super::target;

const CONNECTION_STRING: &str = "127.0.0.1:1337";

/// Configuration for a single GDB endpoint
pub struct GdbInstanceConfiguration {
    /// The core type that will be sent to GDB
    pub core_type: CoreType,
    /// The list of cores to expose.  Each ID corresponds to the value passed to [Session::core()].
    pub cores: Vec<usize>,
    /// The list of [SocketAddr] addresses to bind to
    pub socket_addrs: Vec<SocketAddr>,
}

impl GdbInstanceConfiguration {
    /// Build a GDB configuration from a session object.  All cores are included.
    ///
    /// # Arguments
    ///
    /// * session - the [Session] object to load target information from
    /// * connection_string - The optional connection string to use.  
    ///                       If not specified `localhost:1337` is used.
    ///                       Multiple instances are bound by adding an offset to the supplied port.
    ///
    /// # Returns
    /// Vec with the computed configuration
    pub fn from_session(
        session: &Session,
        connection_string: Option<impl Into<String>>,
    ) -> Vec<Self> {
        let connection_string = connection_string
            .map(|cs| cs.into())
            .unwrap_or_else(|| CONNECTION_STRING.to_owned());

        let addrs: Vec<SocketAddr> = connection_string.to_socket_addrs().unwrap().collect();

        // Build a grouped list of cores by core type
        // GDB only supports one architecture per stub so if we have two core types,
        // such as ARMv7-a + ARMv7-m, we must create two stubs to connect to.
        let groups = session
            .target()
            .cores
            .iter()
            .enumerate()
            .map(|(i, core)| (core.core_type, i))
            .into_group_map();

        // Create a GDB instance for each group, starting at the specified connection and adding one to the port each time
        // For example - consider two groups computed above and an input of localhost:1337.
        // Group 1 will bind to localhost:1337
        // Group 2 will bind to localhost:1338
        let ret = groups
            .iter()
            .enumerate()
            .map(|(i, (core_type, cores))| GdbInstanceConfiguration {
                core_type: *core_type,
                cores: cores.to_vec(),
                socket_addrs: adjust_addrs(&addrs, i),
            })
            .collect();

        ret
    }
}

/// Run a new GDB session.
///
/// # Arguments
///
/// * session - The [Session] to use, protected by a [std::sync::Mutex]
/// * instances - a list of [GdbInstanceConfiguration] objects used to configure the GDB session
///
/// # Remarks
///
/// A default configuration can be created by calling [GdbInstanceConfiguration::from_session()]
pub fn run<'a>(
    session: &Mutex<Session>,
    instances: impl Iterator<Item = &'a GdbInstanceConfiguration>,
) -> Result<()> {
    // Turn our group list into GDB targets
    let mut targets = instances
        .map(|instance| {
            target::RuntimeTarget::new(session, instance.cores.to_vec(), &instance.socket_addrs[..])
        })
        .collect::<Result<Vec<target::RuntimeTarget>, Error>>()?;

    // Process every target in a loop
    loop {
        let mut wait_time = Duration::ZERO;

        for target in targets.iter_mut() {
            wait_time = wait_time.min(target.process()?);
        }

        // Wait until we were asked to check again
        std::thread::sleep(wait_time);
    }
}

/// Given a list of socket addresses, adjust the port by `offset` and return
/// the new values
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
