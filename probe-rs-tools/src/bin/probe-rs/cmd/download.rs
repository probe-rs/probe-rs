use std::path::Path;

use probe_rs::probe::list::Lister;

use crate::util::common_options::BinaryDownloadOptions;
use crate::util::common_options::ProbeOptions;
use crate::util::flash::build_loader;
use crate::util::flash::run_flash_download;
use crate::FirmwareOptions;

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
    format_options: FirmwareOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, probe_options) = self.probe_options.simple_attach(lister)?;

        let loader = build_loader(&mut session, &self.path, self.format_options, None)?;
        run_flash_download(
            &mut session,
            Path::new(&self.path),
            &self.download_options,
            &probe_options,
            loader,
            self.chip_erase,
        )?;

        Ok(())
    }
}
