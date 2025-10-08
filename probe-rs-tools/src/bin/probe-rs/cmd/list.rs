use crate::rpc::client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let probes = client.list_probes().await?;

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                println!("[{num}]: {link}");
            }
        } else {
            println!("No debug probes were found.");
            #[cfg(all(target_os = "linux", feature = "setup-hints"))]
            linux::help_linux();
        }
        Ok(())
    }
}

#[cfg(all(target_os = "linux", feature = "setup-hints"))]
mod linux {
    use std::process::Command;

    const SYSTEMD_SUPPORT_UACCESS_VERSION: usize = 30;
    const UDEV_RULES_PATH: &str = "/etc/udev/rules.d";

    /// Gives the user a hint if they are on Linux.
    ///
    /// Best is to call this only if no probes were found.
    pub(super) fn help_linux() {
        if std::env::var("PROBE_RS_DISABLE_SETUP_HINTS").is_ok() {
            return;
        }

        help_systemd();
        help_udev_rules();
    }

    /// Prints a helptext if udev rules seem to be missing.
    fn help_udev_rules() {
        if !udev_rule_present() {
            tracing::warn!("There seems no probe-rs rule to be installed.");
            tracing::warn!("Read more under https://probe.rs/docs/getting-started/probe-setup/");
            tracing::warn!(
                "If you manage your rules differently, put an empty rule file with 'probe-rs' in the name in {UDEV_RULES_PATH}."
            );
        }
    }

    /// Prints a helptext if udev user groups seem to be missing or wrong.
    fn help_systemd() {
        let systemd_version = systemd_version();

        if systemd_version.unwrap_or_default() < SYSTEMD_SUPPORT_UACCESS_VERSION {
            tracing::warn!(
                "The systemd on your Linux is older than v30, which doesn't support uaccess mechanism"
            );
        }
    }

    /// Returns the systemd version of the current system.
    fn systemd_version() -> Option<usize> {
        let output = match Command::new("systemctl").arg("--version").output() {
            Err(error) => {
                tracing::debug!("Gathering information about relevant user groups failed: {error}");
                return None;
            }
            Ok(child) => child,
        };
        if !output.status.success() {
            tracing::debug!(
                "Gathering information about relevant user groups failed: {:?}",
                output.status.code()
            );
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // First line looks like: "systemd 256 (256.6-1-arch)"
        stdout
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|version| version.parse().ok())
    }

    /// Returns true if there is a probe-rs resembling udev rule file.
    fn udev_rule_present() -> bool {
        let mut files = match std::fs::read_dir(UDEV_RULES_PATH) {
            Err(error) => {
                tracing::debug!("Listing udev rule files at {UDEV_RULES_PATH} failed: {error}");
                return false;
            }
            Ok(files) => files,
        };

        files.any(|p| p.unwrap().path().display().to_string().contains("probe-rs"))
    }
}
