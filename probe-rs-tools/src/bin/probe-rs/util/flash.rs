use crate::rpc::functions::flash::ProgressEvent;
use crate::FormatOptions;

use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::time::Duration;
use std::{path::Path, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use parking_lot::Mutex;
use probe_rs::flashing::{FlashLayout, FlashProgress};
use probe_rs::InstructionSet;
use probe_rs::{
    flashing::{DownloadOptions, FileDownloadError, FlashLoader},
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
    options.preverify = download_options.preverify;

    let flash_layout_output_path = download_options.flash_layout_output_path.clone();

    let pb = if download_options.disable_progressbars {
        None
    } else {
        Some(CliProgressBars::new())
    };

    options.progress = Some(FlashProgress::new(move |event| {
        if let Some(ref path) = flash_layout_output_path {
            if let probe_rs::flashing::ProgressEvent::Initialized { ref phases, .. } = event {
                let mut flash_layout = FlashLayout::default();
                for phase_layout in phases {
                    flash_layout.merge_from(phase_layout.clone());
                }

                // Visualise flash layout to file if requested.
                let visualizer = flash_layout.visualize();
                _ = visualizer.write_svg(path);
            }
        }

        if let Some(ref pb) = pb {
            pb.handle(event.into());
        }
    }));

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
        "     {} in {:.02}s",
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

pub struct ProgressBars {
    pub erase: ProgressBarGroup,
    pub fill: ProgressBarGroup,
    pub program: ProgressBarGroup,
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

    fn idle() -> ProgressStyle {
        ProgressStyle::with_template("{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}]")
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("--")
    }

    fn active() -> ProgressStyle {
        ProgressStyle::with_template("{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}] {bytes:>10} @ {bytes_per_sec:>12} (ETA {eta})")
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("##-")
    }

    fn finished() -> ProgressStyle {
        ProgressStyle::with_template("{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}] {bytes:>10} @ {bytes_per_sec:>12} (took {elapsed})")
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("##")
    }

    pub fn add(&mut self, bar: ProgressBar) {
        if self.append_phase {
            bar.set_message(format!("{} {}", self.message, self.bars.len() + 1));
        } else {
            bar.set_message(self.message.clone());
        }
        bar.set_style(Self::idle());
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
            bar.set_style(Self::active());
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
            bar.set_style(Self::finished());
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

    pub fn mark_start_now(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.reset_elapsed();
            bar.reset_eta();
        }
    }
}

pub struct CliProgressBars {
    multi_progress: MultiProgress,
    progress_bars: Mutex<ProgressBars>,
}

impl CliProgressBars {
    pub fn new() -> Self {
        // Create progress bars.
        let multi_progress = MultiProgress::new();
        logging::set_progress_bar(multi_progress.clone());

        let progress_bars = Mutex::new(ProgressBars {
            erase: ProgressBarGroup::new("      Erasing"),
            fill: ProgressBarGroup::new("Reading flash"),
            program: ProgressBarGroup::new("  Programming"),
        });

        Self {
            multi_progress,
            progress_bars,
        }
    }

    pub fn handle(&self, event: ProgressEvent) {
        let mut progress_bars = self.progress_bars.lock();
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
                        .add(self.multi_progress.add(ProgressBar::new(0)));
                }

                if phases.len() > 1 {
                    progress_bars.erase.append_phase();
                    progress_bars.program.append_phase();
                    progress_bars.fill.append_phase();
                }

                for phase_layout in phases {
                    if restore_unwritten {
                        let fill_size = phase_layout.fills.iter().map(|s| s.size).sum::<u64>();
                        progress_bars
                            .fill
                            .add(self.multi_progress.add(ProgressBar::new(fill_size)));
                    }

                    if !chip_erase {
                        let sector_size = phase_layout.sectors.iter().map(|s| s.size).sum::<u64>();
                        progress_bars
                            .erase
                            .add(self.multi_progress.add(ProgressBar::new(sector_size)));
                    }

                    progress_bars
                        .program
                        .add(self.multi_progress.add(ProgressBar::new(0)));
                }

                // TODO: progress bar for verifying?
            }
            ProgressEvent::StartedProgramming { length } => {
                progress_bars.program.mark_start_now();
                progress_bars.program.set_length(length);
            }
            ProgressEvent::StartedErasing => {
                progress_bars.erase.mark_start_now();
            }
            ProgressEvent::StartedFilling => {
                progress_bars.fill.mark_start_now();
            }
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
    }
}

impl Drop for CliProgressBars {
    fn drop(&mut self) {
        // If we don't do this, the progress bars disappear.
        logging::clear_progress_bar();
    }
}
