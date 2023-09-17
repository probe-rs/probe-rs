use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::fs::File;
use std::time::Duration;
use std::{path::Path, sync::Arc, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::{
    flashing::{DownloadOptions, FlashLoader, FlashProgress, ProgressEvent},
    Session,
};

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    path: &Path,
    download_options: &BinaryDownloadOptions,
    probe_options: &LoadedProbeOptions,
    loader: FlashLoader,
    do_chip_erase: bool,
) -> Result<(), OperationError> {
    // Start timer.
    let instant = Instant::now();

    let mut options = DownloadOptions::default();
    options.keep_unwritten_bytes = download_options.restore_unwritten;
    options.dry_run = probe_options.dry_run();
    options.do_chip_erase = do_chip_erase;
    options.disable_double_buffering = download_options.disable_double_buffering;
    options.verify = download_options.verify;

    if !download_options.disable_progressbars {
        // Create progress bars.
        let multi_progress = MultiProgress::new();
        let style = ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("##-")
                    .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})").expect("Error in progress bar creation. This is a bug, please report it.");

        // Create a new progress bar for the fill progress if filling is enabled.
        let fill_progress = if download_options.restore_unwritten {
            let fill_progress = multi_progress.add(ProgressBar::new(0));
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
        let flash_layout_output_path = download_options.flash_layout_output_path.clone();
        let progress = FlashProgress::new(move |event| {
            use ProgressEvent::*;
            match event {
                Initialized { flash_layout } => {
                    let total_page_size: u32 = flash_layout.pages().iter().map(|s| s.size()).sum();

                    let total_sector_size: u64 =
                        flash_layout.sectors().iter().map(|s| s.size()).sum();

                    let total_fill_size: u64 = flash_layout.fills().iter().map(|s| s.size()).sum();

                    if let Some(fp) = fill_progress.as_ref() {
                        fp.set_length(total_fill_size)
                    }
                    erase_progress.set_length(total_sector_size);
                    program_progress.set_length(total_page_size as u64);
                    let visualizer = flash_layout.visualize();
                    flash_layout_output_path
                        .as_ref()
                        .map(|path| visualizer.write_svg(path));
                }
                StartedProgramming => {
                    program_progress.enable_steady_tick(Duration::from_millis(100));
                    program_progress.reset_elapsed();
                }
                StartedErasing => {
                    erase_progress.enable_steady_tick(Duration::from_millis(100));
                    erase_progress.reset_elapsed();
                }
                StartedFilling => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.enable_steady_tick(Duration::from_millis(100))
                    };
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.reset_elapsed()
                    };
                }
                PageProgrammed { size, .. } => {
                    program_progress.inc(size as u64);
                }
                SectorErased { size, .. } => {
                    erase_progress.inc(size);
                }
                PageFilled { size, .. } => {
                    if let Some(fp) = fill_progress.as_ref() {
                        fp.inc(size)
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
                DiagnosticMessage { .. } => (),
            }
        });

        options.progress = Some(progress);

        loader
            .commit(session, options)
            .map_err(|error| OperationError::FlashingFailed {
                source: error,
                target: Box::new(session.target().clone()),
                target_spec: probe_options.chip(),
                path: path.to_path_buf(),
            })?;
    } else {
        loader
            .commit(session, options)
            .map_err(|error| OperationError::FlashingFailed {
                source: error,
                target: Box::new(session.target().clone()),
                target_spec: probe_options.chip(),
                path: path.to_path_buf(),
            })?;
    }

    // Stop timer.
    let elapsed = instant.elapsed();
    logging::eprintln(format!(
        "    {} in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0,
    ));

    Ok(())
}

/// Builds a new flash loader for the given target and ELF. This
/// will check the ELF for validity and check what pages have to be
/// flashed etc.
pub fn build_elf_flashloader(
    session: &mut Session,
    elf_path: &Path,
) -> Result<FlashLoader, OperationError> {
    let target = session.target();

    // Create the flash loader
    let mut loader = FlashLoader::new(target.memory_map.to_vec(), target.source().clone());

    // Add data from the ELF.
    let mut file = File::open(elf_path).map_err(|error| OperationError::FailedToOpenElf {
        source: error,
        path: elf_path.to_path_buf(),
    })?;

    // Try and load the ELF data.
    loader
        .load_elf_data(&mut file)
        .map_err(OperationError::FailedToLoadElfData)?;

    Ok(loader)
}
