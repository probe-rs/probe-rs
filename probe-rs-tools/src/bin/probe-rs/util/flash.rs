use crate::rpc::functions::flash::{FlashLayout, Operation, ProgressEvent};
use crate::{FormatKind, FormatOptions};

use super::common_options::{BinaryDownloadOptions, LoadedProbeOptions, OperationError};
use super::logging;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::{path::Path, time::Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use parking_lot::Mutex;
use probe_rs::InstructionSet;
use probe_rs::flashing::{BinOptions, ElfOptions, FlashProgress, Format, IdfOptions};
use probe_rs::{
    Session,
    flashing::{DownloadOptions, FileDownloadError, FlashLoader},
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

    options.progress = FlashProgress::new(move |event| {
        if let Some(ref path) = flash_layout_output_path {
            if let probe_rs::flashing::ProgressEvent::FlashLayoutReady {
                flash_layout: ref phases,
            } = event
            {
                let mut flash_layout = FlashLayout::default();
                for phase_layout in phases {
                    flash_layout.merge_from(phase_layout.into());
                }

                // Visualise flash layout to file if requested.
                let visualizer = flash_layout.visualize();
                _ = visualizer.write_svg(path);
            }
        }

        if let Some(ref pb) = pb {
            ProgressEvent::from_library_event(event, |event| pb.handle(event));
        }
    });

    // Start timer.
    let flash_timer = Instant::now();

    loader
        .commit(session, options)
        .map_err(|error| OperationError::FlashingFailed {
            source: Box::new(error),
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
    let format = match format_options.to_format_kind(session.target()) {
        FormatKind::Bin => Format::Bin(BinOptions {
            base_address: format_options.bin_options.base_address,
            skip: format_options.bin_options.skip,
        }),
        FormatKind::Hex => Format::Hex,
        FormatKind::Elf => Format::Elf(ElfOptions {
            skip_sections: format_options.elf_options.skip_section,
        }),
        FormatKind::Uf2 => Format::Uf2,
        FormatKind::Idf => Format::Idf(IdfOptions {
            bootloader: format_options.idf_options.idf_bootloader.map(PathBuf::from),
            partition_table: format_options
                .idf_options
                .idf_partition_table
                .map(PathBuf::from),
            target_app_partition: format_options.idf_options.idf_target_app_partition,
        }),
    };

    probe_rs::flashing::build_loader(session, path, format, image_instruction_set)
}

#[derive(Default)]
pub struct ProgressBars {
    bars: HashMap<Operation, ProgressBarGroup>,
}

impl ProgressBars {
    pub fn get_mut(&mut self, operation: Operation) -> &mut ProgressBarGroup {
        self.bars.entry(operation).or_insert_with(|| {
            let message = match operation {
                Operation::Erase => "Erasing",
                Operation::Fill => "Reading flash",
                Operation::Program => "Programming",
                Operation::Verify => "Verifying",
            };
            ProgressBarGroup::new(format!("{message:>13}"))
        })
    }
}

pub struct ProgressBarGroup {
    message: String,
    bars: Vec<ProgressBar>,
    selected: usize,
}

impl ProgressBarGroup {
    pub fn new(message: String) -> Self {
        Self {
            message,
            bars: vec![],
            selected: 0,
        }
    }

    fn idle(has_length: bool) -> ProgressStyle {
        let template = if has_length {
            "{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}]"
        } else {
            "{msg:.green.bold} {spinner}"
        };
        ProgressStyle::with_template(template)
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("--")
    }

    fn active(has_length: bool) -> ProgressStyle {
        let template = if has_length {
            "{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}] {bytes:>10} @ {bytes_per_sec:>12} (ETA {eta})"
        } else {
            "{msg:.green.bold} {spinner} {elapsed}"
        };
        ProgressStyle::with_template(template)
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("##-")
    }

    fn finished(has_length: bool) -> ProgressStyle {
        let template = if has_length {
            "{msg:.green.bold} {spinner} {percent:>3}% [{bar:20}] {bytes:>10} @ {bytes_per_sec:>12} (took {elapsed})"
        } else {
            "{msg:.green.bold} {spinner} {elapsed}"
        };
        ProgressStyle::with_template(template)
            .expect("Error in progress bar creation. This is a bug, please report it.")
            .tick_chars("⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈✔")
            .progress_chars("##")
    }

    pub fn add(&mut self, bar: ProgressBar) {
        if !self.bars.is_empty() {
            bar.set_message(format!("{} {}", self.message, self.bars.len() + 1));
        } else {
            bar.set_message(self.message.clone());
        }
        bar.set_style(Self::idle(bar.length().is_some()));
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.reset_elapsed();

        self.bars.push(bar);
    }

    pub fn inc(&mut self, size: u64) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.set_style(Self::active(bar.length().is_some()));
            bar.inc(size);
        }
    }

    pub fn abandon(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.abandon();
        }
        self.next();
    }

    pub fn finish(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.set_style(Self::finished(bar.length().is_some()));
            if let Some(length) = bar.length() {
                bar.inc(length.saturating_sub(bar.position()));
            }
            bar.finish();
        }
        self.next();
    }

    pub fn next(&mut self) {
        self.selected += 1;
    }

    pub fn mark_start_now(&mut self) {
        if let Some(bar) = self.bars.get(self.selected) {
            bar.set_style(Self::active(bar.length().is_some()));
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

        let progress_bars = Mutex::new(ProgressBars::default());

        Self {
            multi_progress,
            progress_bars,
        }
    }

    pub fn handle(&self, event: ProgressEvent) {
        let mut progress_bars = self.progress_bars.lock();
        match event {
            ProgressEvent::FlashLayoutReady { .. } => {}

            ProgressEvent::AddProgressBar { operation, total } => {
                let bar = self.multi_progress.add(if let Some(total) = total {
                    // We were promised a length, but in this implementation it
                    // may come later in the Started message. Set to at least 1
                    // to avoid progress bars starting from 100%
                    ProgressBar::new(total.max(1))
                } else {
                    ProgressBar::no_length()
                });
                progress_bars.get_mut(operation).add(bar);
            }
            ProgressEvent::Started(operation) => {
                progress_bars.get_mut(operation).mark_start_now();
            }
            ProgressEvent::Progress { operation, size } => {
                progress_bars.get_mut(operation).inc(size);
            }
            ProgressEvent::Failed(operation) => {
                progress_bars.get_mut(operation).abandon();
            }
            ProgressEvent::Finished(operation) => {
                progress_bars.get_mut(operation).finish();
            }
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
