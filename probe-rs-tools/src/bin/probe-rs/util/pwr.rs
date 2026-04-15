//! Helpers for usb power control

use std::time::Duration;

use anyhow::Result;
use anyhow::anyhow;
use nusb::DeviceInfo;
use probe_rs::probe::DebugProbeSelector;

/// Reset power on a probe
pub async fn power_reset(selector: DebugProbeSelector, delay: Duration) -> Result<()> {
    let dev = nusb::list_devices()
        .await?
        .find(|d| selector.matches(d))
        .ok_or_else(|| anyhow!("device with selector {} not found", selector))?;

    power_reset_impl(&dev, delay).await
}

#[cfg(target_os = "linux")]
/// Reset power on a probe
async fn power_reset_impl(dev: &DeviceInfo, delay: Duration) -> Result<()> {
    use rustix::fd::OwnedFd;
    use rustix::fs::{Mode, OFlags};
    use std::fs::File;
    use std::io::Write;
    use tokio::time::sleep;

    fn disable_f(port_fd: &OwnedFd) -> rustix::io::Result<File> {
        Ok(rustix::fs::openat(
            port_fd,
            "disable",
            OFlags::WRONLY | OFlags::TRUNC,
            Mode::empty(),
        )?
        .into())
    }

    // The USB device goes away when we disable power to it.
    // If we open the port dir we can keep a "handle" to it even if the device goes away, so
    // we can write `disable=0` with openat() to reenable it.
    let port_fd = rustix::fs::open(
        dev.sysfs_path().join("port"),
        OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;

    // disable port power
    disable_f(&port_fd)?.write_all(b"1")?;

    // sleep
    sleep(delay).await;

    // enable port power
    disable_f(&port_fd)?.write_all(b"0")?;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn power_reset_impl(_dev: &DeviceInfo, _delay: Duration) -> Result<()> {
    anyhow::bail!("USB power reset is only supported on linux")
}

#[cfg(all(feature = "remote", not(target_os = "linux")))]
/// Enable power control on all attached hubs
pub async fn power_enable() -> Result<()> {
    anyhow::bail!("USB power reset is only supported on linux")
}

#[cfg(all(feature = "remote", target_os = "linux"))]
/// Enable power control on all attached hubs
pub async fn power_enable() -> Result<()> {
    use std::fs;

    use tokio::time::sleep;
    use tracing::{info, warn};

    const USB_CLASS_HUB: u8 = 0x09;
    const MAX_ITERATIONS: usize = 5;

    info!("Enabling power to all hubs!");

    for iteration in 1..=MAX_ITERATIONS {
        info!(
            "Hub power enable iteration {}/{}",
            iteration, MAX_ITERATIONS
        );
        let mut any_enabled = false;

        for dev in nusb::list_devices().await? {
            // If the device is not a usb hub, continue

            use std::ffi::{OsStr, OsString};
            if dev.class() != USB_CLASS_HUB {
                continue;
            }

            let dev_path = dev.sysfs_path();
            info!("Enabling power for hub at: {dev_path:?}");

            let mut iface_name =
                OsString::from(dev_path.components().next_back().unwrap().as_os_str());
            iface_name.push(OsStr::new(":1.0"));

            let iface_path = dev_path.join(iface_name);
            info!("iface_path: {iface_path:?}");

            // Scan for port directories matching pattern {busdev}-port{number}
            let entries = match fs::read_dir(&iface_path) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!("Failed to read hub directory {iface_path:?}: {e}");
                    continue;
                }
            };

            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();

                // Match directories like "1-1.4-port1", "2-3-port5", etc.
                if name.contains("-port") && entry.path().is_dir() {
                    let disable_path = entry.path().join("disable");

                    // Read current state
                    let current_state = match fs::read_to_string(&disable_path) {
                        Ok(s) => s.trim().to_string(),
                        Err(e) => {
                            warn!("Failed to read disable file for port {name}: {e}");
                            continue;
                        }
                    };

                    if current_state == "0" {
                        // Already enabled, nothing to do
                        continue;
                    }

                    info!("Enabling port: {name} (current state: {current_state})");

                    match fs::write(&disable_path, b"0") {
                        Err(e) => {
                            warn!("Failed to enable port {name}: {e}");
                        }
                        Ok(_) => {
                            info!("Successfully enabled port {name}");
                            any_enabled = true;
                        }
                    }
                }
            }
        }

        if !any_enabled {
            info!("No more ports to enable, done");
            break;
        }

        if iteration < MAX_ITERATIONS {
            info!("Waiting 20s for new hubs to appear...");
            sleep(Duration::from_secs(20)).await;
        }
    }

    Ok(())
}
