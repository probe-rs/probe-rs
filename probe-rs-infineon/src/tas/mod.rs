//! A debug probe that reaches the access hardware through an Infineon TAS server.
//!
//! The Infineon DAS installation runs `TasServer`, which owns the access hardware (for example
//! the DAS JTAG adapter of a ShieldBuddy board) and serves memory accesses over TCP. This probe
//! is a TAS client: it gives probe-rs memory and reset access to the device, which is all
//! TriCore debugging needs.
//!
//! The probe is selected with the Infineon USB vendor ID and a placeholder product ID, followed
//! by the server address and the TAS target name:
//!
//! ```text
//! --probe 058b:0000                                       first target of the local server
//! --probe "058b:0000:DAS JTAG AURDUINO 46YPEADW"          a target of the local server
//! --probe "058b:0000:192.168.1.10:24817/DAS JTAG AURDUINO 46YPEADW"
//! ```
//!
//! Any of these forms also selects the probe from a probe list, as the DAP server does. While
//! the connection is idle (no access for [`IDLE_PING_INTERVAL`]), the probe sends `PING`.

pub mod protocol;

use std::{
    fmt,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use probe_rs::{
    architecture::tricore::bus::{
        BusLimits, BusTransfer, ResetKind, TriCoreBusAccess, TriCoreError,
    },
    probe::{
        DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
        ProbeFactory, WireProtocol, list::ProbeListItem,
    },
};

use protocol::{
    CONNECT_HOT_ATTACH, CONNECT_RESET_AND_HALT, ConInfo, Connection, DEFAULT_PORT, TasError,
    UNLOCK_HOT_ATTACH,
};

/// The USB vendor ID of Infineon, used to select a TAS probe.
pub const TAS_VID: u16 = 0x058B;
/// The placeholder product ID of a TAS probe. A TAS server has no USB identity.
pub const TAS_PID: u16 = 0x0000;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const LIST_TIMEOUT: Duration = Duration::from_millis(200);
const CLIENT_NAME: &str = "probe-rs";

/// Where a TAS probe connects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The host of the TAS server.
    pub host: String,
    /// The TCP port of the TAS server.
    pub port: u16,
    /// The TAS target name. `None` selects the first target.
    pub target: Option<String>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            target: None,
        }
    }
}

impl Endpoint {
    /// Parses the serial number part of a selector: `[host[:port]/][target]`.
    pub fn parse(serial: &str) -> Result<Self, String> {
        let mut endpoint = Self::default();
        let target = match serial.split_once('/') {
            Some((server, target)) => {
                if !server.is_empty() {
                    match server.rsplit_once(':') {
                        Some((host, port)) if !host.contains(':') || host.starts_with('[') => {
                            endpoint.host = host.trim_matches(['[', ']']).to_string();
                            endpoint.port = port
                                .parse()
                                .map_err(|_| format!("invalid TAS server port '{port}'"))?;
                        }
                        _ => endpoint.host = server.to_string(),
                    }
                }
                target
            }
            None => serial,
        };
        if !target.is_empty() {
            endpoint.target = Some(target.to_string());
        }
        Ok(endpoint)
    }

    /// The serial number form of this endpoint, as accepted by [`Endpoint::parse`].
    pub fn serial(&self) -> String {
        format!(
            "{}:{}/{}",
            self.host,
            self.port,
            self.target.as_deref().unwrap_or("")
        )
    }
}

/// The factory of [`TasProbe`]s.
#[derive(Debug)]
pub struct TasFactory;

impl fmt::Display for TasFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Infineon TAS")
    }
}

impl ProbeFactory for TasFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        if selector.vendor_id != TAS_VID || selector.product_id != TAS_PID {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }
        let endpoint = Endpoint::parse(selector.serial_number.as_deref().unwrap_or(""))
            .map_err(DebugProbeError::Other)?;
        Ok(Box::new(TasProbe::new(endpoint)))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        // Only the local server is listed. Other servers are selected explicitly.
        list_server(&Endpoint::default())
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        match selector {
            None => self.list_probes(),
            // The selector names the server, which can be another host, and may name a target
            // in a short form (only the target name) that the listed serial does not match.
            Some(selector) if selector.vendor_id == TAS_VID && selector.product_id == TAS_PID => {
                let Ok(endpoint) = Endpoint::parse(selector.serial_number.as_deref().unwrap_or(""))
                else {
                    return Vec::new();
                };
                list_server(&endpoint)
            }
            Some(_) => Vec::new(),
        }
    }
}

/// The targets of the TAS server of `endpoint`, only its target if it names one.
fn list_server(endpoint: &Endpoint) -> Vec<ProbeListItem> {
    let Ok(mut connection) = Connection::connect(&endpoint.host, endpoint.port, LIST_TIMEOUT)
    else {
        return Vec::new();
    };
    let targets = connection
        .server_connect(CLIENT_NAME, &user_name())
        .and_then(|_| connection.get_targets());
    match targets {
        Ok(targets) => targets
            .into_iter()
            .filter(|target| {
                endpoint
                    .target
                    .as_deref()
                    .is_none_or(|name| target.identifier.eq_ignore_ascii_case(name))
            })
            .map(|target| {
                let listed = Endpoint {
                    target: Some(target.identifier.clone()),
                    ..endpoint.clone()
                };
                ProbeListItem::accessible(DebugProbeInfo::new(
                    format!("TAS {}", target.identifier),
                    TAS_VID,
                    TAS_PID,
                    Some(listed.serial()),
                    &TasFactory,
                    None,
                    false,
                ))
            })
            .collect(),
        Err(error) => {
            tracing::debug!("TAS server at {}:{}: {error}", endpoint.host, endpoint.port);
            Vec::new()
        }
    }
}

fn user_name() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "probe-rs".to_string())
}

/// How long the connection may be idle before the probe sends a `PING`.
pub const IDLE_PING_INTERVAL: Duration = Duration::from_secs(2);

/// The connection and when it was last used, shared with the keep-alive thread.
#[derive(Debug)]
struct Link {
    connection: Connection,
    last_used: Instant,
    pings: usize,
}

/// The keep-alive thread of a connection. It sends `PING` while the connection is idle, as
/// the reference client does, and ends when this is dropped.
#[derive(Debug)]
struct KeepAlive {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl KeepAlive {
    fn start(link: Arc<Mutex<Link>>, interval: Duration) -> Self {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("tas-keepalive".into())
            .spawn(move || {
                let mut failed = false;
                // Wakes up at least every interval; a send or a dropped sender stops it.
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(interval / 2)
                {
                    let Ok(mut link) = link.lock() else { return };
                    if link.last_used.elapsed() < interval {
                        continue;
                    }
                    match link.connection.ping() {
                        Ok(()) => {
                            link.pings += 1;
                            failed = false;
                        }
                        Err(error) if !failed => {
                            tracing::warn!("TAS: PING failed: {error}");
                            failed = true;
                        }
                        Err(_) => {}
                    }
                    link.last_used = Instant::now();
                }
            })
            .ok();
        Self {
            stop: Some(stop),
            thread,
        }
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A connection to a device through a TAS server.
#[derive(Debug)]
pub struct TasProbe {
    endpoint: Endpoint,
    link: Option<Arc<Mutex<Link>>>,
    keep_alive: Option<KeepAlive>,
    idle_ping_interval: Duration,
    info: ConInfo,
    target_name: String,
    device_type: u32,
    connect_under_reset: bool,
}

impl TasProbe {
    /// Creates a probe for `endpoint`. It connects on [`DebugProbe::attach`].
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            link: None,
            keep_alive: None,
            idle_ping_interval: IDLE_PING_INTERVAL,
            info: ConInfo::default(),
            target_name: String::new(),
            device_type: 0,
            connect_under_reset: false,
        }
    }

    /// Sets how long the connection may be idle before a `PING`, for the next attach.
    pub fn set_idle_ping_interval(&mut self, interval: Duration) {
        self.idle_ping_interval = interval;
    }

    /// The number of `PING`s sent on the current connection.
    pub fn pings(&self) -> usize {
        self.link
            .as_ref()
            .and_then(|link| link.lock().ok().map(|link| link.pings))
            .unwrap_or(0)
    }

    /// The name of the TAS target this probe is connected to.
    pub fn target_name(&self) -> &str {
        &self.target_name
    }

    /// Runs `f` on the connection and records the use, so the keep-alive thread stays quiet.
    fn with_connection<T>(
        &mut self,
        f: impl FnOnce(&mut Connection) -> Result<T, TasError>,
    ) -> Result<T, TasError> {
        let link = self.link.as_ref().ok_or(TasError::NotConnected)?;
        let mut link = link
            .lock()
            .map_err(|_| TasError::Protocol("the keep-alive thread panicked".into()))?;
        let result = f(&mut link.connection);
        link.last_used = Instant::now();
        result
    }

    fn disconnect(&mut self) {
        // Stop the thread first: it holds the connection between its pings.
        self.keep_alive = None;
        self.link = None;
    }

    fn connect(&mut self) -> Result<(), TasError> {
        let mut connection =
            Connection::connect(&self.endpoint.host, self.endpoint.port, CONNECT_TIMEOUT)?;
        let server = connection.server_connect(CLIENT_NAME, &user_name())?;
        tracing::info!(
            "Connected to {} {}.{} at {}:{}",
            server.name,
            server.version.0,
            server.version.1,
            self.endpoint.host,
            self.endpoint.port
        );

        let targets = connection.get_targets()?;
        let target = match &self.endpoint.target {
            None => targets.first().ok_or(TasError::NoTargets)?,
            Some(name) => targets
                .iter()
                .find(|t| t.identifier.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    TasError::TargetNotFound(
                        name.clone(),
                        targets
                            .iter()
                            .map(|t| t.identifier.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                })?,
        }
        .clone();
        if target.num_clients > 0 {
            tracing::warn!(
                "{} other TAS client(s) are connected to {}",
                target.num_clients,
                target.identifier
            );
        }

        self.info = connection.session_start(&target.identifier)?;
        connection.set_device_key(UNLOCK_HOT_ATTACH)?;
        let option = if self.connect_under_reset {
            CONNECT_RESET_AND_HALT
        } else {
            CONNECT_HOT_ATTACH
        };
        self.device_type = connection.device_connect(option)?;
        if self.device_type == 0 {
            self.device_type = self.info.device_type;
        }
        tracing::info!(
            "Attached to {} (JTAG ID {:#010x}{})",
            target.identifier,
            self.device_type,
            if self.connect_under_reset {
                ", under reset"
            } else {
                ""
            }
        );

        self.target_name = target.identifier;
        self.disconnect();
        let link = Arc::new(Mutex::new(Link {
            connection,
            last_used: Instant::now(),
            pings: 0,
        }));
        self.keep_alive = Some(KeepAlive::start(link.clone(), self.idle_ping_interval));
        self.link = Some(link);
        Ok(())
    }
}

fn probe_error(error: TasError) -> DebugProbeError {
    DebugProbeError::ProbeSpecific(error.into())
}

impl DebugProbe for TasProbe {
    fn get_name(&self) -> &str {
        "Infineon TAS"
    }

    fn speed_khz(&self) -> u32 {
        // The TAS server sets the JTAG clock.
        0
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        tracing::debug!("TAS: the server sets the interface speed, ignoring {speed_khz} kHz");
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.connect().map_err(|error| match error {
            TasError::Rejected {
                command: "DEVICE_CONNECT",
                ..
            } if !self.connect_under_reset => DebugProbeError::Other(format!(
                "{error}. The device does not accept a hot attach; try --connect-under-reset."
            )),
            other => probe_error(other),
        })
    }

    fn detach(&mut self) -> Result<(), probe_rs::Error> {
        self.disconnect();
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.with_connection(|c| c.device_connect(CONNECT_RESET_AND_HALT))
            .map(|_| ())
            .map_err(probe_error)
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        // The reset is part of the connect: the next attach connects with a reset.
        self.connect_under_reset = true;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn set_reset_line_connected(&mut self, _connected: bool) {
        // The access hardware drives the reset as part of the TAS device connect.
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        tracing::debug!("TAS: the server selects the wire protocol, ignoring {protocol}");
        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        None
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_tricore_bus(&mut self) -> Option<&mut dyn TriCoreBusAccess> {
        if self.link.is_some() {
            Some(self)
        } else {
            None
        }
    }
}

fn bus_error(error: TasError) -> TriCoreError {
    TriCoreError::DebugProbe(probe_error(error))
}

impl TriCoreBusAccess for TasProbe {
    fn execute(&mut self, transfers: &mut [BusTransfer]) -> Result<(), TriCoreError> {
        let info = self.info.clone();
        self.with_connection(|c| c.execute(transfers, &info))
            .map_err(bus_error)
    }

    fn limits(&self) -> BusLimits {
        BusLimits {
            max_transfers: match self.info.pl0_max_num_rw {
                0 => 32,
                n => n as usize,
            },
            max_block_size: 1024,
        }
    }

    fn reset(&mut self, kind: ResetKind) -> Result<(), TriCoreError> {
        match kind {
            ResetKind::ResetAndHalt => self
                .with_connection(|c| c.device_connect(CONNECT_RESET_AND_HALT))
                .map(|_| ())
                .map_err(bus_error),
        }
    }

    fn jtag_id(&mut self) -> Result<u32, TriCoreError> {
        Ok(self.device_type)
    }
}

#[cfg(test)]
mod tests;
