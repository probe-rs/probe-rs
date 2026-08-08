use probe_rs_rpc::flash::{FlashLayout, Operation, ProgressEvent};
use probe_rs_rpc::format::{EspFlashFrequency, EspFlashMode, FormatKind, FormatOptions};

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
use probe_rs::flashing::{
    BinLoader, BinOptions, ElfLoader, ElfOptions, FlashError, FlashProgress, HexLoader,
    ImageLoader, Uf2Loader,
};
use probe_rs::{
    Session, Target,
    flashing::{DownloadOptions, FileDownloadError, FlashLoader},
};
use probe_rs_espressif::image_format::IdfLoader;

/// Performs the flash download with the given loader. Ensure that the loader has the data to load already stored.
/// This function also manages the update and display of progress bars.
pub fn run_flash_download(
    session: &mut Session,
    path: impl AsRef<Path>,
    download_options: &BinaryDownloadOptions,
    probe_options: &LoadedProbeOptions,
    loader: FlashLoader,
) -> Result<(), OperationError> {
    run_flash_download_inner(
        session,
        path.as_ref(),
        download_options,
        probe_options,
        loader,
    )
}

fn run_flash_download_inner(
    session: &mut Session,
    path: &Path,
    download_options: &BinaryDownloadOptions,
    probe_options: &LoadedProbeOptions,
    loader: FlashLoader,
) -> Result<(), OperationError> {
    let mut options = DownloadOptions::default();
    options.keep_unwritten_bytes = download_options.restore_unwritten;
    options.dry_run = probe_options.dry_run();
    options.do_chip_erase = download_options.chip_erase;
    options.disable_double_buffering = download_options.disable_double_buffering;
    options.verify = download_options.verify;
    options.preverify = download_options.preverify;
    options.ram_chunk_size = download_options.ram_chunk_size;

    let pb = if download_options.disable_progressbars {
        None
    } else {
        Some(CliProgressBars::new())
    };

    options.progress = FlashProgress::new(move |event| {
        if let Some(ref path) = download_options.flash_layout_output_path
            && let probe_rs::flashing::ProgressEvent::FlashLayoutReady {
                flash_layout: ref phases,
            } = event
        {
            let mut flash_layout = FlashLayout::default();
            for phase_layout in phases {
                flash_layout.merge_from(
                    crate::rpc::functions::flash::convert::to_wire_flash_layout(phase_layout),
                );
            }

            // Visualise flash layout to file if requested.
            let visualizer = crate::util::visualizer::visualize_flash_layout(&flash_layout);
            _ = visualizer.write_svg(path);
        }

        if let Some(ref pb) = pb {
            crate::rpc::functions::flash::from_library_progress_event(event, |event| {
                pb.handle(event)
            });
        }
    });

    // Start timer.
    let flash_timer = Instant::now();

    let run_flash = if options.preverify {
        match loader.verify(session, &mut options.progress) {
            Ok(_) => false,
            Err(FlashError::Verify) => true,
            Err(error) => {
                return Err(OperationError::FlashingFailed {
                    source: Box::new(error),
                    target: Box::new(session.target().clone()),
                    target_spec: probe_options.chip(),
                    path: path.to_path_buf(),
                });
            }
        }
    } else {
        true
    };

    if run_flash {
        loader
            .commit(session, options)
            .map_err(|error| OperationError::FlashingFailed {
                source: Box::new(error),
                target: Box::new(session.target().clone()),
                target_spec: probe_options.chip(),
                path: path.to_path_buf(),
            })?;
    }

    // If we don't do this, the progress bars disappear.
    logging::clear_progress_bar();

    logging::eprintln(format!(
        "     {} in {:.02}s",
        "Finished".green().bold(),
        flash_timer.elapsed().as_secs_f32(),
    ));

    Ok(())
}

fn espflash_flash_frequency(freq: EspFlashFrequency) -> espflash::flasher::FlashFrequency {
    match freq {
        EspFlashFrequency::_12Mhz => espflash::flasher::FlashFrequency::_12Mhz,
        EspFlashFrequency::_15Mhz => espflash::flasher::FlashFrequency::_15Mhz,
        EspFlashFrequency::_16Mhz => espflash::flasher::FlashFrequency::_16Mhz,
        EspFlashFrequency::_20Mhz => espflash::flasher::FlashFrequency::_20Mhz,
        EspFlashFrequency::_24Mhz => espflash::flasher::FlashFrequency::_24Mhz,
        EspFlashFrequency::_26Mhz => espflash::flasher::FlashFrequency::_26Mhz,
        EspFlashFrequency::_30Mhz => espflash::flasher::FlashFrequency::_30Mhz,
        EspFlashFrequency::_40Mhz => espflash::flasher::FlashFrequency::_40Mhz,
        EspFlashFrequency::_48Mhz => espflash::flasher::FlashFrequency::_48Mhz,
        EspFlashFrequency::_60Mhz => espflash::flasher::FlashFrequency::_60Mhz,
        EspFlashFrequency::_80Mhz => espflash::flasher::FlashFrequency::_80Mhz,
    }
}

fn espflash_flash_mode(mode: EspFlashMode) -> espflash::flasher::FlashMode {
    match mode {
        EspFlashMode::Qio => espflash::flasher::FlashMode::Qio,
        EspFlashMode::Qout => espflash::flasher::FlashMode::Qout,
        EspFlashMode::Dio => espflash::flasher::FlashMode::Dio,
        EspFlashMode::Dout => espflash::flasher::FlashMode::Dout,
    }
}

pub fn resolve_format_kind(kind: FormatKind, target: &Target) -> FormatKind {
    kind.resolve_default_format(target.default_format.as_deref())
}

fn format_options_image_loader(options: &FormatOptions, target: &Target) -> Box<dyn ImageLoader> {
    match resolve_format_kind(options.binary_format, target) {
        FormatKind::Target => unreachable!(),
        FormatKind::Bin => Box::new(BinLoader(BinOptions {
            base_address: options.bin_options.base_address,
            skip: options.bin_options.skip,
        })),

        FormatKind::Hex => Box::new(HexLoader),
        FormatKind::Elf => Box::new(ElfLoader(ElfOptions {
            skip_sections: options.elf_options.skip_section.clone(),
        })),
        FormatKind::Uf2 => Box::new(Uf2Loader),

        FormatKind::Idf => Box::new(IdfLoader {
            bootloader: options
                .idf_options
                .idf_bootloader
                .as_ref()
                .map(PathBuf::from),
            partition_table: options
                .idf_options
                .idf_partition_table
                .as_ref()
                .map(PathBuf::from),
            target_app_partition: options.idf_options.idf_target_app_partition.clone(),
            flash_frequency: options
                .idf_options
                .idf_flash_freq
                .map(espflash_flash_frequency),
            flash_mode: options.idf_options.idf_flash_mode.map(espflash_flash_mode),
        }),
    }
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
    let loader = format_options_image_loader(&format_options, session.target());
    probe_rs::flashing::build_loader(session, path, loader, image_instruction_set)
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
                Operation::Ram => "Writing RAM",
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
            ProgressEvent::DiagnosticMessage { message } => {
                logging::println(message);
            }
        }
    }
}

impl Drop for CliProgressBars {
    fn drop(&mut self) {
        // If we don't do this, the progress bars disappear.
        logging::clear_progress_bar();
    }
}
