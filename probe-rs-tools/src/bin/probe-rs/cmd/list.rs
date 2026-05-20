use crate::rpc::client::RpcClient;

#[derive(clap::Parser)]
pub struct Cmd {
    /// Output as JSON for programmatic consumption
    #[arg(long)]
    json: bool,
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let probes = client.list_probes().await?;

        if self.json {
            let entries: Vec<serde_json::Value> = probes
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    serde_json::json!({
                        "id": i,
                        "name": p.identifier,
                        "vid": format!("{:04x}", p.vendor_id),
                        "pid": format!("{:04x}", p.product_id),
                        "sn": p.serial_number,
                        "type": p.probe_type,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string(&entries)?);
        } else if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                println!("[{num}]: {link}");
            }
        } else {
            println!("No debug probes were found.");
        }
        Ok(())
    }
}
