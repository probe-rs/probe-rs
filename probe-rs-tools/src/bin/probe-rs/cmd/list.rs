use itertools::Itertools;
use serde::{Deserialize, Serialize};

use crate::cmd::remote::{
    functions::list_probes::{DebugProbeEntry, ListProbes},
    SessionInterface,
};

#[derive(clap::Parser, Serialize, Deserialize)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(
        self,
        config: &crate::Config,
        iface: &mut impl SessionInterface,
    ) -> anyhow::Result<()> {
        let probes = iface.run_call(ListProbes::new()).await?;

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, entry) in probes.iter().enumerate() {
                let sets = config
                    .parameter_sets
                    .iter()
                    .filter_map(|d| match d.selector {
                        Some(ref selector) if device_matches(selector, entry) => Some(&d.name),

                        _ => None,
                    })
                    .join(", ");

                if !sets.is_empty() {
                    println!("[{num}]: {entry} (included in: {sets})");
                } else {
                    println!("[{num}]: {entry}");
                }
            }
        } else {
            println!("No debug probes were found.");
        }
        Ok(())
    }
}

fn device_matches(selector: &probe_rs::probe::DebugProbeSelector, link: &DebugProbeEntry) -> bool {
    selector.product_id == link.product_id
        && selector.vendor_id == link.vendor_id
        && selector.serial_number == link.serial_number
}
