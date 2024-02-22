//! Interact with a target device using RTT.

use crate::config::TargetSelector;
use crate::rtt::{Channels, Error, Rtt, RttChannel, ScanRegion};
use crate::Core;
use crate::Error as ProbeError;
use crate::Session;
use crate::{probe::list::Lister, Permissions};

use std::sync::mpsc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/// Result of spawning a thread to handle RTT channels.
pub struct RttHostSpawnResult {
    /// Handle to the spawned thread.
    pub handle: std::io::Result<std::thread::JoinHandle<Result<(), Error>>>,

    /// Sender for host to target data.
    pub host_to_target: Sender<Vec<u8>>,

    /// Receiver for target to host data.
    pub target_to_host: Receiver<Vec<u8>>,
}

/// Attach behavior used when spawning channels.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Attach {
    /// Attach to the target while it is running.
    #[default]
    Running,

    /// Reset the target before attaching to it.
    UnderReset,
}

/// Parse a 'ScanRegion' from a string.
pub fn parse_scan_region(
    mut src: &str,
) -> Result<ScanRegion, Box<dyn std::error::Error + Send + Sync + 'static>> {
    src = src.trim();
    if src.is_empty() {
        return Ok(ScanRegion::Ram);
    }

    let parts = src
        .split("..")
        .map(|p| {
            if p.starts_with("0x") || p.starts_with("0X") {
                u32::from_str_radix(&p[2..], 16)
            } else {
                p.parse()
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    match *parts.as_slice() {
        [addr] => Ok(ScanRegion::Exact(addr)),
        [start, end] => Ok(ScanRegion::Range(start..end)),
        _ => Err("Invalid range: multiple '..'s".into()),
    }
}

/// Connect to a target using a debug probe and exchange data with it using RTT.
pub struct RttHost {
    session: Session,
    scan_region: Option<ScanRegion>,
}

impl RttHost {
    /// Create a new 'RttHost' instance with the given probe number and chip name.
    pub fn new(
        probe_number: usize,
        chip: Option<&str>,
        scan_region: Option<ScanRegion>,
    ) -> Result<Self, Error> {
        let lister = Lister::new();
        let probes = lister.list_all();

        if probes.is_empty() {
            return Err(Error::Probe(ProbeError::UnableToOpenProbe(
                "No debug probes available",
            )));
        }

        if probe_number >= probes.len() {
            return Err(Error::Probe(ProbeError::UnableToOpenProbe(
                "Probe does not exist.",
            )));
        }

        // Attaching to the probe
        let probe = probes[probe_number]
            .open(&lister)
            .map_err(|e| Error::Probe(ProbeError::from(e)))?;
        let target_selector = TargetSelector::from(chip);
        let session = probe.attach(target_selector, Permissions::default())?;

        Ok(RttHost {
            session,
            scan_region,
        })
    }

    fn core(&mut self) -> Result<Core, Error> {
        Ok(self.session.core(0)?)
    }

    fn rtt(&mut self) -> Result<Rtt, Error> {
        let memory_map = self.session.target().memory_map.clone();
        let mut core = self.session.core(0)?;

        // Attaching to RTT
        let region = match self.scan_region.clone() {
            Some(region) => region,
            None => parse_scan_region("").map_err(|e| {
                Error::Probe(ProbeError::Other(anyhow::anyhow!(
                    "Error parsing region: {e}"
                )))
            })?,
        };

        let rtt = Rtt::attach_region(&mut core, &memory_map, &region)?;

        Ok(rtt)
    }

    /// Returns the memory address of the control block in target memory.
    pub fn ctrl_block(&mut self) -> Result<u32, Error> {
        let rtt = self.rtt()?;
        Ok(rtt.ptr())
    }

    /// Returns a list of up channels as friendly strings.
    pub fn up_channel_list(&mut self) -> Result<Vec<String>, Error> {
        let mut rtt = self.rtt()?;
        Ok(Self::channels_as_friendly_strings(rtt.up_channels()))
    }

    /// Returns a list of down channels as friendly strings.
    pub fn down_channel_list(&mut self) -> Result<Vec<String>, Error> {
        let mut rtt = self.rtt()?;
        Ok(Self::channels_as_friendly_strings(rtt.down_channels()))
    }

    /// Internal function to convert a list of channels to a list of friendly strings.
    fn channels_as_friendly_strings(
        channels: &Channels<impl RttChannel + std::fmt::Display>,
    ) -> Vec<String> {
        let mut result = vec![];

        if channels.is_empty() {
            result.push("(none)".to_string());
        } else {
            for chan in channels.iter() {
                result.push(format!("{chan}"));
            }
        }

        result
    }

    /// Create a channel pair for the host to exchange data with the target.
    ///
    /// Spawn a thread that:
    ///   - Buffers data bound for the target via the returned `Sender<Vec<u8>>` and writes to the target device.
    ///   - Reads data from the target device and buffers it via the returned `Receiver<Vec<u8>>` for the host to receive.
    pub fn spawn_channels(mut self, attach: Attach) -> Result<RttHostSpawnResult, Error> {
        let mut rtt = self.rtt()?;

        // Find channels
        let opt_up = Some(0_usize);
        let up_channel = if let Some(up) = opt_up {
            let chan = rtt.up_channels().take(up);

            if chan.is_none() {
                return Err(Error::UpChannelDoesNotExist(up));
            }

            chan
        } else {
            rtt.up_channels().take(0)
        };

        let opt_down = Some(0_usize);
        let down_channel = if let Some(down) = opt_down {
            let chan = rtt.down_channels().take(down);

            if chan.is_none() {
                return Err(Error::DownChannelDoesNotExist(down));
            }

            chan
        } else {
            rtt.down_channels().take(0)
        };

        if attach == Attach::UnderReset {
            let mut core = self.core()?;
            core.reset()?;
        }

        // Setup channels for data exchange
        let target_to_host = mpsc::channel::<Vec<u8>>();
        let host_to_target = mpsc::channel::<Vec<u8>>();

        // The 'Core' type is not thread safe, so we need to set up all the communication before
        // passing ownership of 'self' to the spawned thread.
        let handle = thread::Builder::new().name("rtt_host".to_string()).spawn(
            move || -> Result<(), Error> {
                let mut up_buf = [0u8; 1024];
                let mut down_buf: Vec<u8> = vec![];

                let mut core = self.core()?;

                loop {
                    if let Some(up_channel) = up_channel.as_ref() {
                        let count = up_channel.read(&mut core, up_buf.as_mut())?;

                        match target_to_host.0.send(up_buf[..count].to_vec()) {
                            Ok(_) => {}
                            Err(err) => {
                                return Err(Error::Probe(ProbeError::Other(anyhow::anyhow!(
                                    "Error sending to host: {err}"
                                ))));
                            }
                        }
                    }

                    if let (Some(down_channel), host_to_target) =
                        (down_channel.as_ref(), &host_to_target.1)
                    {
                        if let Ok(bytes) = host_to_target.try_recv() {
                            down_buf.extend_from_slice(bytes.as_slice());
                        }

                        if !down_buf.is_empty() {
                            let count = down_channel.write(&mut core, down_buf.as_mut())?;

                            if count > 0 {
                                down_buf.drain(..count);
                            }
                        }
                    }
                }
            },
        );

        Ok(RttHostSpawnResult {
            handle,
            host_to_target: host_to_target.0,
            target_to_host: target_to_host.1,
        })
    }
}
