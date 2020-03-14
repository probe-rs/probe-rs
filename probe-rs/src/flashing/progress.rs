use std::time::Duration;

/// Worker for reporting flash progress
///
/// ```
/// use probe_rs::flash::FlashProgress;
///
/// // Print events
/// let progress = FlashProgress::new(|event| println!("Event: {:#?}", event));
/// ```
pub struct FlashProgress {
    handler: Box<dyn Fn(ProgressEvent)>,
}

impl FlashProgress {
    /// Create a new `FlashProgress` worker
    pub fn new(handler: impl Fn(ProgressEvent) + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    pub(crate) fn emit(&self, event: ProgressEvent) {
        (self.handler)(event);
    }

    pub(crate) fn initialized(&self, total_pages: usize, total_sector_size: usize, page_size: u32) {
        self.emit(ProgressEvent::Initialized {
            total_pages,
            total_sector_size,
            page_size,
        });
    }

    pub(crate) fn started_flashing(&self) {
        self.emit(ProgressEvent::StartedFlashing);
    }

    pub(crate) fn started_erasing(&self) {
        self.emit(ProgressEvent::StartedErasing);
    }

    pub(crate) fn page_programmed(&self, size: u32, time: Duration) {
        self.emit(ProgressEvent::PageFlashed { size, time });
    }

    pub(crate) fn sector_erased(&self, size: u32, time: Duration) {
        self.emit(ProgressEvent::SectorErased { size, time });
    }

    pub(crate) fn failed_programming(&self) {
        self.emit(ProgressEvent::FailedProgramming);
    }

    pub(crate) fn finished_programming(&self) {
        self.emit(ProgressEvent::FinishedProgramming);
    }

    pub(crate) fn failed_erasing(&self) {
        self.emit(ProgressEvent::FailedErasing);
    }

    pub(crate) fn finished_erasing(&self) {
        self.emit(ProgressEvent::FinishedErasing);
    }
}

/// Possible events from the flashing process
#[derive(Debug)]
pub enum ProgressEvent {
    /// Flashing process initialized
    Initialized {
        total_pages: usize,
        total_sector_size: usize,
        page_size: u32,
    },
    /// Programming of flash has started
    StartedFlashing,
    /// Erase of flash has started
    StartedErasing,
    /// A flash page has been programmed successfully
    PageFlashed { size: u32, time: Duration },
    /// A sector has been erased
    SectorErased { size: u32, time: Duration },
    /// Programming of flash failed
    FailedProgramming,
    /// Programming of flash has finished successfully
    FinishedProgramming,
    /// Erase of flash failed
    FailedErasing,
    /// Erase of flash finished successfully
    FinishedErasing,
}
