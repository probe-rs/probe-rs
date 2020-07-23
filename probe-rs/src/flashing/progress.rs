use super::FlashLayout;
use std::time::Duration;

/// A structure to manage the flashing procedure progress reporting.
///
/// This struct stores a handler closure which will be called everytime an event happens during the flashing process.
/// Such an event can be start or finish of the flashing procedure or a progress report, as well as some more events.
///
/// ```
/// use probe_rs::flashing::FlashProgress;
///
/// // Print events
/// let progress = FlashProgress::new(|event| println!("Event: {:#?}", event));
/// ```

pub struct FlashProgress {
    handler: Box<dyn Fn(ProgressEvent)>,
}

impl FlashProgress {
    /// Create a new `FlashProgress` structure with a given `handler` to be called on events.
    pub fn new(handler: impl Fn(ProgressEvent) + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// Emit a flashing progress event.
    fn emit(&self, event: ProgressEvent) {
        (self.handler)(event);
    }

    /// Signalize that the flashing algorithm was set up and is initialized.
    pub(super) fn initialized(&self, flash_layout: FlashLayout) {
        self.emit(ProgressEvent::Initialized { flash_layout });
    }

    /// Signalize that the erasing procedure started.
    pub(super) fn started_erasing(&self) {
        self.emit(ProgressEvent::StartedErasing);
    }

    /// Signalize that the filling procedure started.
    pub(super) fn started_filling(&self) {
        self.emit(ProgressEvent::StartedFilling);
    }

    /// Signalize that the programing procedure started.
    pub(super) fn started_programming(&self) {
        self.emit(ProgressEvent::StartedProgramming);
    }

    /// Signalize that the page programming procedure has made progress.
    pub(super) fn page_programmed(&self, size: u32, time: Duration) {
        self.emit(ProgressEvent::PageProgrammed { size, time });
    }

    /// Signalize that the sector erasing procedure has made progress.
    pub(super) fn sector_erased(&self, size: u32, time: Duration) {
        self.emit(ProgressEvent::SectorErased { size, time });
    }

    /// Signalize that the page filling procedure has made progress.
    pub(super) fn page_filled(&self, size: u32, time: Duration) {
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
/// If an erorr occurs in any stage, one of the `Failed*` event will be returned,
/// and no further events will be returned.
#[derive(Debug)]
pub enum ProgressEvent {
    Initialized {
        flash_layout: FlashLayout,
    },
    /// Filling of flash pages has started.
    StartedFilling,
    /// A page has been filled successfully.
    PageFilled {
        size: u32,
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
        size: u32,
        time: Duration,
    },
    /// Erasing of the flash has failed.
    FailedErasing,
    /// Erasing of the flash has finished successfully.
    FinishedErasing,
    /// Programming of the flash has started.
    StartedProgramming,
    /// A flash page has been programmed successfully.
    PageProgrammed {
        size: u32,
        time: Duration,
    },
    /// Programming of the flash failed.
    FailedProgramming,
    /// Programming of the flash has finished successfully.
    FinishedProgramming,
}
