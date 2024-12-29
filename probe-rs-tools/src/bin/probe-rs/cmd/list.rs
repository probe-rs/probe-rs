use itertools::Itertools;
use probe_rs::probe::list::Lister;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub fn run(self, lister: &Lister, config: &crate::Config) -> anyhow::Result<()> {
        let probes = lister.list_all();

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                let sets = config
                    .parameter_sets
                    .iter()
                    .filter_map(|d| match d.selector {
                        Some(ref selector) if device_matches(selector, link) => Some(&d.name),

                        _ => None,
                    })
                    .join(", ");

                if !sets.is_empty() {
                    println!("[{num}]: {link} (included in: {sets})");
                } else {
                    println!("[{num}]: {link}");
                }
            }
        } else {
            println!("No debug probes were found.");
        }
        Ok(())
    }
}

fn device_matches(
    selector: &probe_rs::probe::DebugProbeSelector,
    link: &probe_rs::probe::DebugProbeInfo,
) -> bool {
    selector.product_id == link.product_id
        && selector.vendor_id == link.vendor_id
        && selector.serial_number == link.serial_number
}
