use std::fs::File;
use std::path::Path;

use anyhow::Context;
use probe_rs::flashing::FileDownloadError;
use probe_rs::flashing::Format;

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
    pub fn run(self) -> anyhow::Result<()> {
        let (mut session, probe_options) = self.probe_options.simple_attach()?;

        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
        };

        let mut loader = session.target().flash_loader();

        let format = self.format_options.into_format(session.target())?;
        match format {
            Format::Bin(options) => loader.load_bin_data(&mut file, options),
            Format::Elf => loader.load_elf_data(&mut file),
            Format::Hex => loader.load_hex_data(&mut file),
            Format::Idf(options) => loader.load_idf_data(&mut session, &mut file, options),
            Format::Uf2 => loader.load_uf2_data(&mut file),
        }?;

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
