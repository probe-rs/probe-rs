use anyhow::{Context, Result};
use probe_rs::flashing::FileDownloadError;
use probe_rs_cli_util::common_options::{CargoOptions, FlashOptions, ProbeOptions};
use probe_rs_cli_util::flash::run_flash_download;
use std::fs::File;
use std::path::Path;

pub fn run(common: ProbeOptions, path: &str, chip_erase: bool) -> Result<()> {
    let mut session = common.simple_attach()?;

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
    };

    let mut loader = session.target().flash_loader();
    loader.load_elf_data(&mut file)?;

    run_flash_download(
        &mut session,
        Path::new(path),
        &FlashOptions {
            version: false,
            list_chips: false,
            list_probes: false,
            disable_progressbars: false,
            reset_halt: false,
            log: None,
            restore_unwritten: false,
            flash_layout_output_path: None,
            elf: None,
            work_dir: None,
            cargo_options: CargoOptions::default(),
            probe_options: common,
        },
        loader,
        chip_erase,
    )?;

    let mut core = session.core(0)?;
    core.reset()?;

    Ok(())
}
