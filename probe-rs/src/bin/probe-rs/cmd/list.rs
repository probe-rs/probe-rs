use probe_rs::Probe;

#[derive(clap::Parser)]
pub struct Cmd {}

impl Cmd {
    pub fn run(self) -> anyhow::Result<()> {
        let probes = Probe::list_all();

        if !probes.is_empty() {
            println!("The following debug probes were found:");
            for (num, link) in probes.iter().enumerate() {
                println!("[{num}]: {link:?}");
            }
        } else {
            println!("No debug probes were found.");
        }
        Ok(())
    }
}
