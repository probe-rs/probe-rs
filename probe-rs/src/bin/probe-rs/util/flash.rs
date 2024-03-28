use crate::FormatOptions;

use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::fs::File;
use std::time::Duration;
use std::{path::Path, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::{
    flashing::{
        DownloadOptions, FileDownloadError, FlashLoader, FlashProgress, Format, ProgressEvent,
    },
    Session,
};

use anyhow::Context;

fn init_progress_bar(bar: &ProgressBar) {
    let style = bar.style().progress_chars("##-");
    bar.set_style(style);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar.reset_elapsed();
}

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    path: impl AsRef<Path>,
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
        logging::set_progress_bar(multi_progress.clone());

        let style = ProgressStyle::default_bar()
                    .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
                    .progress_chars("--")
                    .template("{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})").expect("Error in progress bar creation. This is a bug, please report it.");

        // Create a new progress bar for the fill progress if filling is enabled.
        let fill_progress = if download_options.restore_unwritten {
            let fill_progress = multi_progress.add(ProgressBar::new(0));
            fill_progress.set_style(style.clone());
            fill_progress.set_message("Reading flash");
            Some(fill_progress)
        } else {
            None
        };

        // Create a new progress bar for the erase progress.
        let erase_progress = multi_progress.add(ProgressBar::new(0));
        erase_progress.set_style(style.clone());
        erase_progress.set_message("      Erasing");

        // Create a new progress bar for the program progress.
        let program_progress = multi_progress.add(ProgressBar::new(0));
        program_progress.set_style(style);
        program_progress.set_message("  Programming");

        // Register callback to update the progress.
        let flash_layout_output_path = download_options.flash_layout_output_path.clone();
        let progress = FlashProgress::new(move |event| match event {
            ProgressEvent::Initialized { flash_layout } => {
                if let Some(fp) = fill_progress.as_ref() {
                    let total_fill_size: u64 = flash_layout.fills().iter().map(|s| s.size()).sum();
                    fp.set_length(total_fill_size);
                }

                let total_sector_size: u64 = flash_layout.sectors().iter().map(|s| s.size()).sum();
                erase_progress.set_length(total_sector_size);

                let visualizer = flash_layout.visualize();
                flash_layout_output_path
                    .as_ref()
                    .map(|path| visualizer.write_svg(path));
            }
            ProgressEvent::StartedProgramming { length } => {
                init_progress_bar(&program_progress);
                program_progress.set_length(length);
            }
            ProgressEvent::StartedErasing => {
                init_progress_bar(&erase_progress);
            }
            ProgressEvent::StartedFilling => {
                if let Some(fp) = fill_progress.as_ref() {
                    init_progress_bar(fp);
                }
            }
            ProgressEvent::PageProgrammed { size, .. } => {
                program_progress.inc(size as u64);
            }
            ProgressEvent::SectorErased { size, .. } => {
                erase_progress.inc(size);
            }
            ProgressEvent::PageFilled { size, .. } => {
                if let Some(fp) = fill_progress.as_ref() {
                    fp.inc(size);
                }
            }
            ProgressEvent::FailedErasing => {
                erase_progress.abandon();
                program_progress.abandon();
            }
            ProgressEvent::FinishedErasing => {
                erase_progress.finish();
            }
            ProgressEvent::FailedProgramming => {
                program_progress.abandon();
            }
            ProgressEvent::FinishedProgramming => {
                program_progress.finish();
            }
            ProgressEvent::FailedFilling => {
                if let Some(fp) = fill_progress.as_ref() {
                    fp.abandon();
                }
            }
            ProgressEvent::FinishedFilling => {
                if let Some(fp) = fill_progress.as_ref() {
                    fp.finish();
                }
            }
            ProgressEvent::DiagnosticMessage { .. } => (),
        });

        options.progress = Some(progress);
    }

    loader
        .commit(session, options)
        .map_err(|error| OperationError::FlashingFailed {
            source: error,
            target: Box::new(session.target().clone()),
            target_spec: probe_options.chip(),
            path: path.as_ref().to_path_buf(),
        })?;

    // If we don't do this, the progress bars disappear.
    logging::clear_progress_bar();

    // Stop timer.
    let elapsed = instant.elapsed();
    logging::eprintln(format!(
        "    {} in {}s",
        "Finished".green().bold(),
        elapsed.as_millis() as f32 / 1000.0,
    ));

    Ok(())
}

/// Builds a new flash loader for the given target and path. This
/// will check the path for validity and check what pages have to be
/// flashed etc.
pub fn build_loader(
    session: &mut Session,
    path: impl AsRef<Path>,
    format_options: FormatOptions,
) -> anyhow::Result<FlashLoader> {
    // Create the flash loader
    let mut loader = session.target().flash_loader();

    // Add data from the BIN.
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
    };

    let format = format_options.into_format(session.target())?;
    match format {
        Format::Bin(options) => loader.load_bin_data(&mut file, options),
        Format::Elf => loader.load_elf_data(&mut file),
        Format::Hex => loader.load_hex_data(&mut file),
        Format::Idf(options) => loader.load_idf_data(session, &mut file, options),
        Format::Uf2 => loader.load_uf2_data(&mut file),
    }?;

    Ok(loader)
}
