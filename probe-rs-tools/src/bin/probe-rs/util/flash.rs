use crate::FormatOptions;

use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::cell::RefCell;
use std::time::Duration;
use std::{path::Path, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use probe_rs::flashing::FlashLayout;
use probe_rs::InstructionSet;
use probe_rs::{
    flashing::{DownloadOptions, FileDownloadError, FlashLoader, FlashProgress, ProgressEvent},
    Session,
};

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
                                phase_layout.fills().iter().map(|s| s.size()).sum::<u64>();
                            progress_bars
                                .fill
                                .add(multi_progress.add(ProgressBar::new(fill_size)));
                        }

                        if !chip_erase {
                            let sector_size =
                                phase_layout.sectors().iter().map(|s| s.size()).sum::<u64>();
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
    format_options: FormatOptions,
    image_instruction_set: Option<InstructionSet>,
) -> Result<FlashLoader, FileDownloadError> {
    let format = format_options.into_format(session.target());

    probe_rs::flashing::build_loader(session, path, format, image_instruction_set)
}

struct ProgressBars {
    erase: ProgressBarGroup,
    fill: ProgressBarGroup,
    program: ProgressBarGroup,
}

pub struct ProgressBarGroup {
    message: String,
    bars: Vec<ProgressBar>,
    selected: usize,
    append_phase: bool,
}

impl ProgressBarGroup {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
            bars: vec![],
            selected: 0,
            append_phase: false,
        }
    }

    pub fn add(&mut self, bar: ProgressBar) {
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

    pub fn set_length(&mut self, length: u64) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.set_length(length);
        }
    }

    pub fn inc(&mut self, size: u64) {
        if let Some(bar) = self.bars.get(self.selected) {
            let style = bar.style().progress_chars("##-");
            bar.set_style(style);
            bar.inc(size);
        }
    }

    pub fn len(&mut self) -> u64 {
        self.bars
            .get(self.selected)
            .and_then(|bar| bar.length())
            .unwrap_or(0)
    }

    pub fn abandon(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.abandon();
        }
        self.next();
    }

    pub fn finish(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.finish();
        }
        self.next();
    }

    pub fn next(&mut self) {
        self.selected += 1;
    }

    pub fn append_phase(&mut self) {
        self.append_phase = true;
    }
}
