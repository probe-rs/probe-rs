use std::sync::Mutex;
use std::time::Duration;

use probe_rs_cli_util::common_options::ProbeOptions;

pub fn run_gdb_server(
    common: ProbeOptions,
    connection_string: Option<&str>,
    reset_halt: bool,
) -> anyhow::Result<()> {
    let mut session = common.simple_attach()?;

    if reset_halt {
        session
            .core(0)?
            .reset_and_halt(Duration::from_millis(100))?;
    }

    let gdb_connection_string = connection_string.unwrap_or("localhost:1337");

    let instances = probe_rs_gdb_server::GdbInstanceConfiguration::from_session(
        &session,
        Some(gdb_connection_string.to_owned()),
    );

    for instance in instances.iter() {
        println!(
            "Firing up GDB stub for {:?} cores at {:?}",
            instance.core_type, instance.socket_addrs
        );
    }

    let session = Mutex::new(session);

    if let Err(e) = probe_rs_gdb_server::run(&session, instances.iter()) {
        eprintln!("During the execution of GDB an error was encountered:");
        eprintln!("{:?}", e);
    }

    Ok(())
}
