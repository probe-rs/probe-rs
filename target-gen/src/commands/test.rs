use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::{
    flashing::{DownloadOptions, FlashLoader, FlashProgress},
    Permissions, Session,
};

use super::export::{cmd_export, DEFINITION_EXPORT_PATH};

pub fn cmd_test() -> Result<()> {
    // Generate the binary
    cmd_export()?;

    probe_rs::config::add_target_from_yaml(Path::new(DEFINITION_EXPORT_PATH))?;
    let mut session =
        probe_rs::Session::auto_attach("algorithm-test", Permissions::new().allow_erase_all())?;

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

    // Create progress bars.
    let multi_progress = MultiProgress::new();
    let style = ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("##-")
                    .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10}").expect("Error in progress bar creation. This is a bug, please report it.");

    // Create a new progress bar for the erase progress.
    let erase_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
    {
        probe_rs_cli_util::logging::set_progress_bar(erase_progress.clone());
    }
    erase_progress.set_style(style.clone());
    erase_progress.set_message("     Erasing sectors");

    // Create a new progress bar for the program progress.
    let program_progress = multi_progress.add(ProgressBar::new(0));
    program_progress.set_style(style);
    program_progress.set_message(" Programming pages  ");

    // Register callback to update the progress.
    let progress = FlashProgress::new(move |event| {
        use probe_rs::flashing::ProgressEvent::*;
        match event {
            Initialized { flash_layout } => {
                let total_page_size: u32 = flash_layout.pages().iter().map(|s| s.size()).sum();

                let total_sector_size: u64 = flash_layout.sectors().iter().map(|s| s.size()).sum();

                erase_progress.set_length(total_sector_size);
                program_progress.set_length(total_page_size as u64);
            }
            StartedProgramming => {
                program_progress.enable_steady_tick(Duration::from_millis(100));
                program_progress.reset_elapsed();
            }
            StartedErasing => {
                erase_progress.enable_steady_tick(Duration::from_millis(100));
                erase_progress.reset_elapsed();
            }
            StartedFilling => {}
            PageProgrammed { size, .. } => {
                program_progress.inc(size as u64);
            }
            SectorErased { size, .. } => {
                erase_progress.inc(size);
            }
            PageFilled { .. } => {}
            FailedErasing => {
                erase_progress.abandon();
                program_progress.abandon();
            }
            FinishedErasing => {
                erase_progress.finish();
            }
            FailedProgramming => {
                program_progress.abandon();
            }
            FinishedProgramming => {
                program_progress.finish();
            }
            FailedFilling => {}
            FinishedFilling => {}
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
