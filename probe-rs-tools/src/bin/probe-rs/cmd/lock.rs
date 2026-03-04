use crate::util::common_options::ProbeOptions;
use probe_rs::config::Registry;
use probe_rs::probe::list::Lister;

/// Lock the debug port of the target device.
///
/// This command enables debug port protection on the target device, preventing
/// unauthorized debug access. The exact behavior is vendor-specific and may
/// require a power cycle to take effect.
///
/// Requires `--allow-permanent-debug-lock` to be passed as a safety measure.
#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    /// Lock level (vendor-specific). Omit to use the default level.
    #[arg(long)]
    pub level: Option<String>,

    /// List the supported lock levels for the connected device and exit.
    #[arg(long)]
    pub list_levels: bool,
}

/// Warning: Do not implement that as a remote command (RPC) as exposing
/// permanent operations like this over RPC is a bad idea.
impl Cmd {
    pub fn run(self, registry: &mut Registry, lister: &Lister) -> anyhow::Result<()> {
        let common_options = self.common.load(registry)?;
        let probe = common_options.attach_probe(lister)?;
        let target = common_options.get_target_selector()?;
        let mut session = common_options.attach_session(probe, target)?;

        if self.list_levels {
            println!("Available Lock Level:");
            for level in session.supported_lock_levels()? {
                println!(
                    "{}{}",
                    level.name,
                    if level.is_permanent {
                        " (permanent)"
                    } else {
                        ""
                    }
                );
                println!("  {}", level.description);
            }
            return Ok(());
        }

        session.lock_device(self.level.as_deref())?;
        println!(
            "Debug port locked successfully. A power cycle may be required for the lock to take effect."
        );
        Ok(())
    }
}
