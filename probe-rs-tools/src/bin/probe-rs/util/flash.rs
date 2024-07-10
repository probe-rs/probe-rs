use crate::FirmwareOptions;

use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::cell::RefCell;
use std::fs::File;
use std::time::Duration;
use std::{path::Path, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::flashing::platform::Platform;
use probe_rs::flashing::FlashLayout;
use probe_rs::InstructionSet;
use probe_rs::{
    flashing::{DownloadOptions, FileDownloadError, FlashLoader, FlashProgress, ProgressEvent},
    Session,
};

use anyhow::Context;

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

        let progress_bars = RefCell::new(ProgressBars {
            erase: ProgressBarGroup::new("      Erasing"),
            fill: ProgressBarGroup::new("Reading flash"),
            program: ProgressBarGroup::new("  Programming"),
        });

        // Register callback to update the progress.
        let flash_layout_output_path = download_options.flash_layout_output_path.clone();
        let progress = FlashProgress::new(move |event| {
            let mut progress_bars = progress_bars.borrow_mut();

            match event {
                ProgressEvent::Initialized {
                    chip_erase,
                    phases,
                    restore_unwritten,
                } => {
                    // Build progress bars.
                    if chip_erase {
                        progress_bars
                            .erase
                            .add(multi_progress.add(ProgressBar::new(0)));
                    }

                    if phases.len() > 1 {
                        progress_bars.erase.append_phase();
                        progress_bars.program.append_phase();
                        progress_bars.fill.append_phase();
                    }

                    let mut flash_layout = FlashLayout::default();
                    for phase_layout in phases {
                        if restore_unwritten {
                            let fill_size =
                                flash_layout.fills().iter().map(|s| s.size()).sum::<u64>();
                            progress_bars
                                .fill
                                .add(multi_progress.add(ProgressBar::new(fill_size)));
                        }

                        if !chip_erase {
                            let sector_size =
                                flash_layout.sectors().iter().map(|s| s.size()).sum::<u64>();
                            progress_bars
                                .erase
                                .add(multi_progress.add(ProgressBar::new(sector_size)));
                        }

                        progress_bars
                            .program
                            .add(multi_progress.add(ProgressBar::new(0)));

                        flash_layout.merge_from(phase_layout);
                    }

                    // TODO: progress bar for verifying?

                    // Visualise flash layout to file if requested.
                    let visualizer = flash_layout.visualize();
                    flash_layout_output_path
                        .as_ref()
                        .map(|path| visualizer.write_svg(path));
                }
                ProgressEvent::StartedProgramming { length } => {
                    progress_bars.program.set_length(length);
                }
                ProgressEvent::StartedErasing => {}
                ProgressEvent::StartedFilling => {}
                ProgressEvent::PageProgrammed { size, .. } => {
                    progress_bars.program.inc(size as u64);
                }
                ProgressEvent::SectorErased { size, .. } => progress_bars.erase.inc(size),
                ProgressEvent::PageFilled { size, .. } => progress_bars.fill.inc(size),
                ProgressEvent::FailedErasing => {
                    progress_bars.erase.abandon();
                    progress_bars.program.abandon();
                }
                ProgressEvent::FinishedErasing => progress_bars.erase.finish(),
                ProgressEvent::FailedProgramming => progress_bars.program.abandon(),
                ProgressEvent::FinishedProgramming => progress_bars.program.finish(),
                ProgressEvent::FailedFilling => progress_bars.fill.abandon(),
                ProgressEvent::FinishedFilling => progress_bars.fill.finish(),
                ProgressEvent::DiagnosticMessage { .. } => {}
            }
        });

        options.progress = Some(progress);
    }

    // Start timer.
    let flash_timer = Instant::now();

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

    logging::eprintln(format!(
        "    {} in {}s",
        "Finished".green().bold(),
        flash_timer.elapsed().as_secs_f32(),
    ));

    Ok(())
}

/// Builds a new flash loader for the given target and path. This
/// will check the path for validity and check what pages have to be
/// flashed etc.
pub fn build_loader(
    session: &mut Session,
    path: impl AsRef<Path>,
    format_options: FirmwareOptions,
    image_instruction_set: Option<InstructionSet>,
) -> anyhow::Result<FlashLoader> {
    // Create the flash loader
    let mut loader = session.target().flash_loader();

    // Add data from the BIN.
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)).context("Failed to open binary file."),
    };

    let platform = match format_options.platform() {
        Some(platform) => platform,
        None => Platform::from_optional(session.target().default_platform.as_deref())
            .map(|result| result.expect("Unknown platform. This should not have passed tests."))
            .unwrap_or_default()
            .default_loader(),
    };

    let format = format_options.into_format(session.target())?;
    loader.load_image(session, &mut file, format, platform, image_instruction_set)?;

    Ok(loader)
}

struct ProgressBars {
    erase: ProgressBarGroup,
    fill: ProgressBarGroup,
    program: ProgressBarGroup,
}

struct ProgressBarGroup {
    message: String,
    bars: Vec<ProgressBar>,
    selected: usize,
    append_phase: bool,
}

impl ProgressBarGroup {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
            bars: vec![],
            selected: 0,
            append_phase: false,
        }
    }

    fn add(&mut self, bar: ProgressBar) {
        let msg_template = "{msg:.green.bold} {spinner} [{elapsed_precise}] [{wide_bar}] {bytes:>8}/{total_bytes:>8} @ {bytes_per_sec:>10} (eta {eta:3})";
        let style = ProgressStyle::default_bar()
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("--")
            .template(msg_template)
            .expect("Error in progress bar creation. This is a bug, please report it.");

        if self.append_phase {
            bar.set_message(format!("{} {}", self.message, self.bars.len() + 1));
        } else {
            bar.set_message(self.message.clone());
        }
        bar.set_style(style);
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.reset_elapsed();

        self.bars.push(bar);
    }

    fn set_length(&mut self, length: u64) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.set_length(length);
        }
    }

    fn inc(&mut self, size: u64) {
        if let Some(bar) = self.bars.get(self.selected) {
            let style = bar.style().progress_chars("##-");
            bar.set_style(style);
            bar.inc(size);
        }
    }

    fn abandon(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.abandon();
        }
        self.next();
    }

    fn finish(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.finish();
        }
        self.next();
    }

    fn next(&mut self) {
        self.selected += 1;
    }

    fn append_phase(&mut self) {
        self.append_phase = true;
    }
}
