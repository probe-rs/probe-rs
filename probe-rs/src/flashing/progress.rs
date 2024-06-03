use super::FlashLayout;
use std::{sync::Arc, time::Duration};

/// A structure to manage the flashing procedure progress reporting.
///
/// This struct stores a handler closure which will be called every time an event happens during the flashing process.
/// Such an event can be start or finish of the flashing procedure or a progress report, as well as some more events.
///
/// # Example
///
/// ```
/// use probe_rs::flashing::FlashProgress;
///
/// // Print events
/// let progress = FlashProgress::new(|event| println!("Event: {:#?}", event));
/// ```
#[derive(Clone)]
pub struct FlashProgress {
    handler: Arc<dyn Fn(ProgressEvent)>,
}

impl FlashProgress {
    /// Create a new `FlashProgress` structure with a given `handler` to be called on events.
    pub fn new(handler: impl Fn(ProgressEvent) + 'static) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }

    /// Create a new `FlashProgress` structure with an empty handler.
    pub fn empty() -> Self {
        Self {
            handler: Arc::new(|_| {}),
        }
    }

    /// Emit a flashing progress event.
    fn emit(&self, event: ProgressEvent) {
        (self.handler)(event);
    }

    /// Signalize that the flashing algorithm was set up and is initialized.
    pub(super) fn initialized(
        &self,
        chip_erase: bool,
        restore_unwritten: bool,
        phases: Vec<FlashLayout>,
    ) {
        self.emit(ProgressEvent::Initialized {
            chip_erase,
            restore_unwritten,
            phases,
        });
    }

    /// Signalize that the erasing procedure started.
    pub(super) fn started_erasing(&self) {
        self.emit(ProgressEvent::StartedErasing);
    }

    /// Signalize that the filling procedure started.
    pub(super) fn started_filling(&self) {
        self.emit(ProgressEvent::StartedFilling);
    }

    /// Signalize that the programming procedure started.
    pub(super) fn started_programming(&self, length: u64) {
        self.emit(ProgressEvent::StartedProgramming { length });
    }

    /// Signalize that the page programming procedure has made progress.
    pub(super) fn page_programmed(&self, size: u32, time: Duration) {
        self.emit(ProgressEvent::PageProgrammed { size, time });
    }

    /// Signalize that the sector erasing procedure has made progress.
    pub(super) fn sector_erased(&self, size: u64, time: Duration) {
        self.emit(ProgressEvent::SectorErased { size, time });
    }

    /// Signalize that the page filling procedure has made progress.
    pub(super) fn page_filled(&self, size: u64, time: Duration) {
        self.emit(ProgressEvent::PageFilled { size, time });
    }

    /// Signalize that the programming procedure failed.
    pub(super) fn failed_programming(&self) {
        self.emit(ProgressEvent::FailedProgramming);
    }

    /// Signalize that the programming procedure completed successfully.
    pub(super) fn finished_programming(&self) {
        self.emit(ProgressEvent::FinishedProgramming);
    }

    /// Signalize that the erasing procedure failed.
    pub(super) fn failed_erasing(&self) {
        self.emit(ProgressEvent::FailedErasing);
    }

    /// Signalize that the erasing procedure completed successfully.
    pub(super) fn finished_erasing(&self) {
        self.emit(ProgressEvent::FinishedErasing);
    }

    /// Signalize that the filling procedure failed.
    pub(super) fn failed_filling(&self) {
        self.emit(ProgressEvent::FailedFilling);
    }

    /// Signalize that the filling procedure completed successfully.
    pub(super) fn finished_filling(&self) {
        self.emit(ProgressEvent::FinishedFilling);
    }

    pub(super) fn message(&self, message: String) {
        self.emit(ProgressEvent::DiagnosticMessage { message });
    }
}

/// Possible events during the flashing process.
///
/// If flashing works without problems, the events will arrive in the
/// following order:
///
/// * `Initialized`
/// * `StartedFilling`
/// * `PageFilled` for every page
/// * `FinishedFilling`
/// * `StartedErasing`
/// * `SectorErased` for every sector
/// * `FinishedErasing`
/// * `StartedProgramming`
/// * `PageProgrammed` for every page
/// * `FinishedProgramming`
///
/// If an error occurs in any stage, one of the `Failed*` event will be returned,
/// and no further events will be returned.
#[derive(Debug)]
pub enum ProgressEvent {
    /// The flash layout has been built and the flashing procedure was initialized.
    Initialized {
        /// Whether the chip erase feature is enabled.
        /// If this is true, the chip will be erased before any other operation. No separate erase
        /// progress bars are necessary in this case.
        chip_erase: bool,

        /// The layout of the flash contents as it will be used by the flash procedure, grouped by
        /// phases (fill, erase, program sequences).
        /// This is an exact report of what the flashing procedure will do during the flashing process.
        phases: Vec<FlashLayout>,

        /// Whether the unwritten flash contents will be restored after erasing.
        restore_unwritten: bool,
    },
    /// Filling of flash pages has started.
    StartedFilling,
    /// A page has been filled successfully.
    /// This does not mean the page has been programmed yet.
    /// Only its contents are determined at this point!
    PageFilled {
        /// The size of the page in bytes.
        size: u64,
        /// The time it took to fill this flash page.
        time: Duration,
    },
    /// Filling of the pages has failed.
    FailedFilling,
    /// Filling of the pages has finished successfully.
    FinishedFilling,
    /// Erasing of flash has started.
    StartedErasing,
    /// A sector has been erased successfully.
    SectorErased {
        /// The size of the sector in bytes.
        size: u64,
        /// The time it took to erase this sector.
        time: Duration,
    },
    /// Erasing of the flash has failed.
    FailedErasing,
    /// Erasing of the flash has finished successfully.
    FinishedErasing,
    /// Programming of the flash has started.
    StartedProgramming {
        /// The total length of the data to be programmed in bytes.
        length: u64,
    },
    /// A flash page has been programmed successfully.
    PageProgrammed {
        /// The size of this page in bytes.
        size: u32,
        /// The time it took to program this page.
        time: Duration,
    },
    /// Programming of the flash failed.
    FailedProgramming,
    /// Programming of the flash has finished successfully.
    FinishedProgramming,
    /// a message was received from the algo.
    DiagnosticMessage {
        /// The message that was emitted.
        message: String,
    },
}
