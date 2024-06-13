use probe_rs::{
    flashing::{erase_all, FlashProgress},
    probe::list::Lister,
};

use crate::util::common_options::ProbeOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister)?;

        erase_all(&mut session, FlashProgress::empty())?;

        Ok(())
    }
}
