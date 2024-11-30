use probe_rs::probe::list::Lister;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let probes = lister.list_all().await;

        if !probes.is_empty() {
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
