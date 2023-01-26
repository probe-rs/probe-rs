use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
use std::{cell::RefCell, path::PathBuf};

use anyhow::Result;
use colored::Colorize;
use probe_rs::{
    flashing::{DownloadOptions, FlashLoader, FlashProgress},
    Permissions, Session,
};

use super::export::{cmd_export, DEFINITION_EXPORT_PATH};

pub fn cmd_run(target_artifact: PathBuf) -> Result<()> {
    // Generate the binary
    cmd_export(target_artifact)?;

    probe_rs::config::add_target_from_yaml(Path::new(DEFINITION_EXPORT_PATH))?;
    let mut session =
        probe_rs::Session::auto_attach("nrf51822_xxaa", Permissions::new().allow_erase_all())?;

    let mut loader = session.target().flash_loader();
    let data = (0..0x401)
        .into_iter()
        .map(|n| (n % 256) as u8)
        .collect::<Vec<_>>();
    loader.add_data(0x0, &data)?;

    run_flash_download(&mut session, loader, false, false)?;

    Ok(())
}

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    loader: FlashLoader,
    do_chip_erase: bool,
    disable_double_buffering: bool,
) -> Result<()> {
    // Start timer.
    let instant = Instant::now();

    let mut download_option = DownloadOptions::default();
    download_option.keep_unwritten_bytes = false;
    download_option.do_chip_erase = do_chip_erase;
    download_option.disable_double_buffering = disable_double_buffering;

    // Register callback to update the progress.
    let t = Rc::new(RefCell::new(Instant::now()));
    let progress = FlashProgress::new(move |event| {
        use probe_rs::flashing::ProgressEvent::*;
        match event {
            StartedProgramming => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            StartedErasing => {
                let mut t = t.borrow_mut();
                *t = Instant::now();
            }
            FailedErasing => {
                println!("Failed erasing in {:?}", t.borrow().elapsed());
            }
            FinishedErasing => {
                println!("Finished erasing in {:?}", t.borrow().elapsed());
            }
            FailedProgramming => {
                println!("Failed programming in {:?}", t.borrow().elapsed());
            }
            FinishedProgramming => {
                println!("Finished programming in {:?}", t.borrow().elapsed());
            }
            Rtt { channel, message } => {
                if message.ends_with('\n') {
                    print!("RTT({channel}): {message}");
                } else {
                    println!("RTT({channel}): {message}");
                }
            }
            _ => (),
        }
    });

    download_option.progress = Some(&progress);

    loader.commit(session, download_option)?;

    // Stop timer.
    let elapsed = instant.elapsed();
    probe_rs_cli_util::logging::println(format!(
        "    {} in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0,
    ));

    Ok(())
}
