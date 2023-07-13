use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use probe_rs::flashing::{FileDownloadError, Format};
use time::UtcOffset;

use crate::util::common_options::{CargoOptions, FlashOptions, ProbeOptions};
use crate::util::flash::run_flash_download;
use crate::util::rtt;
use crate::FormatOptions;

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    pub(crate) common: ProbeOptions,

    /// The path to the ELF file to flash and run
    pub(crate) path: String,

    /// Whether to erase the entire chip before downloading
    #[clap(long)]
    pub(crate) chip_erase: bool,

    /// Disable double-buffering when downloading flash.  If downloading times out, try this option.
    #[clap(long = "disable-double-buffering")]
    pub(crate) disable_double_buffering: bool,

    #[clap(flatten)]
    pub(crate) format_options: FormatOptions,
}

impl Cmd {
    pub fn run(self, timestamp_offset: UtcOffset) -> anyhow::Result<()> {
        let mut session = self.common.simple_attach()?;

        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
        };

        let mut loader = session.target().flash_loader();

        let format = self.format_options.into_format()?;
        match format {
            Format::Bin(options) => loader.load_bin_data(&mut file, options),
            Format::Elf => loader.load_elf_data(&mut file),
            Format::Hex => loader.load_hex_data(&mut file),
            Format::Idf(options) => loader.load_idf_data(&mut session, &mut file, options),
        }?;

        run_flash_download(
            &mut session,
            Path::new(&self.path),
            &FlashOptions {
                disable_progressbars: false,
                disable_double_buffering: self.disable_double_buffering,
                reset_halt: false,
                log: None,
                restore_unwritten: false,
                flash_layout_output_path: None,
                elf: None,
                work_dir: None,
                cargo_options: CargoOptions::default(),
                probe_options: self.common,
            },
            loader,
            self.chip_erase,
        )?;

        let rtt_config = rtt::RttConfig::default();

        let memory_map = session.target().memory_map.clone();

        let mut core = session.core(0)?;
        core.reset()?;

        let mut rtta = match rtt::attach_to_rtt(
            &mut core,
            &memory_map,
            Path::new(&self.path),
            &rtt_config,
            timestamp_offset,
        ) {
            Ok(target_rtt) => Some(target_rtt),
            Err(error) => {
                log::error!("{:?} Continuing without RTT... ", error);
                None
            }
        };

        if let Some(rtta) = &mut rtta {
            let mut stdout = std::io::stdout();
            loop {
                for (_ch, data) in rtta.poll_rtt_fallible(&mut core)? {
                    stdout.write_all(data.as_bytes())?;
                }

                // Poll RTT with a frequency of 10 Hz
                //
                // If the polling frequency is too high,
                // the USB connection to the probe can become unstable.
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        Ok(())
    }
}
