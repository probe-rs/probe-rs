use crate::rpc::client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let probes = client.list_probes().await?;

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                let marker = if link.inaccessible {
                    " (inaccessible)"
                } else {
                    ""
                };
                println!("[{num}]: {link}{marker}");
            }
        } else {
            println!("No debug probes were found.");
        }

        // Nudge the user about their setup if we found nothing, or found a probe we can't access.
        if probes.is_empty() || probes.iter().any(|probe| probe.inaccessible) {
            crate::util::setup_hints::print_setup_hints();
        }

        Ok(())
    }
}
