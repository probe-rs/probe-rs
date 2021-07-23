use crate::common_options::{FlashOptions, OperationError};
use crate::{
    indicatif::{MultiProgress, ProgressBar, ProgressStyle},
    logging,
};

use std::{path::Path, sync::Arc, time::Instant};

use colored::Colorize;
use probe_rs::{
    flashing::{DownloadOptions, FlashLoader, FlashProgress, ProgressEvent},
    Session,
};

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    path: &Path,
    opt: &FlashOptions,
    loader: FlashLoader,
) -> Result<(), OperationError> {
    // Start timer.
    let instant = Instant::now();

    let mut download_option = DownloadOptions::default();
    download_option.keep_unwritten_bytes = opt.restore_unwritten;
    download_option.dry_run = opt.probe_options.dry_run;

    if !opt.disable_progressbars {
        // Create progress bars.
        let multi_progress = MultiProgress::new();
        let style = ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("##-")
                    .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})");

        // Create a new progress bar for the fill progress if filling is enabled.
        let fill_progress = if opt.restore_unwritten {
            let fill_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
            fill_progress.set_style(style.clone());
            fill_progress.set_message("     Reading flash  ");
            Some(fill_progress)
        } else {
            None
        };

        // Create a new progress bar for the erase progress.
        let erase_progress = Arc::new(multi_progress.add(ProgressBar::new(0)));
        {
            logging::set_progress_bar(erase_progress.clone());
        }
        erase_progress.set_style(style.clone());
        erase_progress.set_message("     Erasing sectors");

        // Create a new progress bar for the program progress.
        let program_progress = multi_progress.add(ProgressBar::new(0));
        program_progress.set_style(style);
        program_progress.set_message(" Programming pages  ");

        // Register callback to update the progress.
        let flash_layout_output_path = opt.flash_layout_output_path.clone();
        let progress = FlashProgress::new(move |event| {
            use ProgressEvent::*;
            match event {
                Initialized { flash_layout } => {
                    let total_page_size: u32 = flash_layout.pages().iter().map(|s| s.size()).sum();

                    let total_sector_size: u32 =
                        flash_layout.sectors().iter().map(|s| s.size()).sum();

                    let total_fill_size: u32 = flash_layout.fills().iter().map(|s| s.size()).sum();

                    if let Some(fp) = fill_progress.as_ref() {
                        fp.set_length(total_fill_size as u64)
                    }
                    erase_progress.set_length(total_sector_size as u64);
                    program_progress.set_length(total_page_size as u64);
                    let visualizer = flash_layout.visualize();
                    flash_layout_output_path
                        .as_ref()
                        .map(|path| visualizer.write_svg(path));
                }
                StartedProgramming => {
                    program_progress.enable_steady_tick(100);
                    program_progress.reset_elapsed();
                }
                StartedErasing => {
                    erase_progress.enable_steady_tick(100);
                    erase_progress.reset_elapsed();
                }
                StartedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.enable_steady_tick(100)
                    };
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.reset_elapsed()
                    };
                }
                PageProgrammed { size, .. } => {
                    program_progress.inc(size as u64);
                }
                SectorErased { size, .. } => {
                    erase_progress.inc(size as u64);
                }
                PageFilled { size, .. } => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.inc(size as u64)
                    };
                }
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
                FailedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.abandon()
                    };
                }
                FinishedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.finish()
                    };
                }
            }
        });

        // Make the multi progresses print.
        // indicatif requires this in a separate thread as this join is a blocking op,
        // but is required for printing multiprogress.
        let progress_thread_handle = std::thread::spawn(move || {
            multi_progress.join().unwrap();
        });

        download_option.progress = Some(&progress);

        loader.commit(session, download_option).map_err(|error| {
            OperationError::FlashingFailed {
                source: error,
                target: session.target().clone(),
                target_spec: opt.probe_options.chip.clone(),
                path: format!("{}", path.display()),
            }
        })?;

        // We don't care if we cannot join this thread.
        let _ = progress_thread_handle.join();
    } else {
        loader.commit(session, download_option).map_err(|error| {
            OperationError::FlashingFailed {
                source: error,
                target: session.target().clone(),
                target_spec: opt.probe_options.chip.clone(),
                path: format!("{}", path.display()),
            }
        })?;
    }

    // Stop timer.
    let elapsed = instant.elapsed();
    logging::println(format!(
        "    {} in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0,
    ));

    Ok(())
}
