use std::str::FromStr;

use anyhow::{anyhow, bail};
use probe_rs::{DebugProbeInfo, Probe};

use crate::cli;

const NO_PROBE_FOUND_ERR: &str = "no probe was found.\n
Common reasons for this are faulty cables or missing permissions.
For detailed instructions, visit: https://github.com/knurling-rs/probe-run/tree/2f138c3#troubleshooting";

pub fn open(opts: &cli::Opts) -> Result<Probe, anyhow::Error> {
    let all_probes = Probe::list_all();
    let filtered_probes = if let Some(probe_opt) = opts.probe.as_deref() {
        let selector = probe_opt.parse()?;
        filter(&all_probes, &selector)
    } else {
        all_probes
    };

    if filtered_probes.is_empty() {
        bail!("{}", NO_PROBE_FOUND_ERR)
    }

    log::debug!("found {} probes", filtered_probes.len());

    if filtered_probes.len() > 1 {
        print(&filtered_probes);
        bail!("more than one probe found; use --probe to specify which one to use");
    }

    let mut probe = filtered_probes[0].open()?;
    log::debug!("opened probe");

    if let Some(speed) = opts.speed {
        probe.set_speed(speed)?;
    }

    Ok(probe)
}

pub fn print(probes: &[DebugProbeInfo]) {
    if !probes.is_empty() {
        println!("the following probes were found:");
        probes
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!("[{}]: {:?}", num, link));
    } else {
        println!("Error: {}", NO_PROBE_FOUND_ERR);
    }
}

fn filter(probes: &[DebugProbeInfo], selector: &ProbeFilter) -> Vec<DebugProbeInfo> {
    probes
        .iter()
        .filter(|probe| {
            if let Some((vid, pid)) = selector.vid_pid {
                if probe.vendor_id != vid || probe.product_id != pid {
                    return false;
                }
            }

            if let Some(serial) = &selector.serial {
                if probe.serial_number.as_deref() != Some(serial) {
                    return false;
                }
            }

            true
        })
        .cloned()
        .collect()
}

struct ProbeFilter {
    vid_pid: Option<(u16, u16)>,
    serial: Option<String>,
}

impl FromStr for ProbeFilter {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s.split(':').collect::<Vec<_>>();
        match *parts {
            [serial] => Ok(Self {
                vid_pid: None,
                serial: Some(serial.to_string()),
            }),
            [vid, pid] => Ok(Self {
                vid_pid: Some((u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?)),
                serial: None,
            }),
            [vid, pid, serial] => Ok(Self {
                vid_pid: Some((u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?)),
                serial: Some(serial.to_string()),
            }),
            _ => Err(anyhow!("invalid probe filter")),
        }
    }
}
