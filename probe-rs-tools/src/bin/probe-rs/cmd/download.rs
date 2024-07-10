use std::path::Path;

use probe_rs::probe::list::Lister;

use crate::util::common_options::BinaryDownloadOptions;
use crate::util::common_options::ProbeOptions;
use crate::util::flash::run_flash_download;
use crate::FormatOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    probe_options: ProbeOptions,

    /// The path to the file to be downloaded to the flash
    path: String,

    /// Whether to erase the entire chip before downloading
    #[clap(long)]
    chip_erase: bool,

    #[clap(flatten)]
    download_options: BinaryDownloadOptions,

    #[clap(flatten)]
    format_options: FormatOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, probe_options) = self.probe_options.simple_attach(lister)?;

        run_flash_download(
            &mut session,
            Path::new(&self.path),
            &self.download_options,
            &probe_options,
            self.chip_erase,
            self.format_options,
            None,
        )?;

        Ok(())
    }
}
