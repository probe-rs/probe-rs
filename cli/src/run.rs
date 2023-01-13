use anyhow::{Context, Result};
use probe_rs::flashing::FileDownloadError;
use probe_rs_cli_util::common_options::{CargoOptions, FlashOptions, ProbeOptions};
use probe_rs_cli_util::flash::run_flash_download;
use probe_rs_cli_util::rtt;
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub fn run(
    common: ProbeOptions,
    path: &str,
    chip_erase: bool,
    disable_double_buffering: bool,
) -> Result<()> {
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
            list_chips: false,
            list_probes: false,
            disable_progressbars: false,
            disable_double_buffering,
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

    let rtt_config = rtt::RttConfig::default();

    let memory_map = session.target().memory_map.clone();

    let mut core = session.core(0)?;
    core.reset()?;

    let mut rtta = match rtt::attach_to_rtt(&mut core, &memory_map, Path::new(path), &rtt_config) {
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
                stdout.write_all(data.as_bytes()).unwrap();
            }
        }
    }

    Ok(())
}
