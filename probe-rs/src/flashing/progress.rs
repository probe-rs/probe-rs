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
pub struct FlashProgress<'a> {
    handler: Arc<dyn Fn(ProgressEvent) + Send + Sync + 'a>,
}

impl<'a> FlashProgress<'a> {
    /// Create a new `FlashProgress` structure with a given `handler` to be called on events.
    pub fn new(handler: impl Fn(ProgressEvent) + Send + Sync + 'a) -> Self {
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
    pub fn emit(&self, event: ProgressEvent) {
        (self.handler)(event);
    }

    // --- Methods for emitting specific kinds of events.

    /// Signal that the flashing algorithm was set up and is initialized.
    pub(super) fn initialized(&self, phases: Vec<FlashLayout>) {
        self.emit(ProgressEvent::FlashLayoutReady {
            flash_layout: phases,
        });
    }

    /// Signal that a new progress bar should be created.
    pub(super) fn add_progress_bar(&self, operation: ProgressOperation, total: Option<u64>) {
        self.emit(ProgressEvent::AddProgressBar { operation, total });
    }

    /// Signal that the procedure started.
    pub(super) fn started(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Started(operation));
    }

    /// Signal that the procedure has made progress.
    pub(super) fn progressed(&self, operation: ProgressOperation, size: u64, time: Duration) {
        self.emit(ProgressEvent::Progress {
            operation,
            size,
            time,
        });
    }

    /// Signal that the procedure failed.
    pub(super) fn failed(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Failed(operation));
    }

    /// Signal that the procedure completed successfully.
    pub(super) fn finished(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Finished(operation));
    }

    /// Signal that a flashing algorithm produced a diagnostic message.
    pub(super) fn message(&self, message: String) {
        self.emit(ProgressEvent::DiagnosticMessage { message });
    }

    // --- Methods for emitting events for a specific operation.

    /// Signal that the erasing procedure started.
    pub(super) fn started_erasing(&self) {
        self.started(ProgressOperation::Erase);
    }

    /// Signal that the filling procedure started.
    pub(super) fn started_filling(&self) {
        self.started(ProgressOperation::Fill);
    }

    /// Signal that the programming procedure started.
    pub(super) fn started_programming(&self) {
        self.started(ProgressOperation::Program);
    }

    /// Signal that the verifying procedure started.
    pub(crate) fn started_verifying(&self) {
        self.started(ProgressOperation::Verify);
    }

    /// Signal that the sector erasing procedure has made progress.
    pub(super) fn sector_erased(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Erase, size, time);
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) fn page_filled(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Fill, size, time);
    }

    /// Signal that the page programming procedure has made progress.
    pub(super) fn page_programmed(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Program, size, time);
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) fn page_verified(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Verify, size, time);
    }

    /// Signal that the erasing procedure failed.
    pub(super) fn failed_erasing(&self) {
        self.failed(ProgressOperation::Erase);
    }

    /// Signal that the filling procedure failed.
    pub(super) fn failed_filling(&self) {
        self.failed(ProgressOperation::Fill);
    }

    /// Signal that the programming procedure failed.
    pub(super) fn failed_programming(&self) {
        self.failed(ProgressOperation::Program);
    }

    /// Signal that the verifying procedure failed.
    pub(super) fn failed_verifying(&self) {
        self.failed(ProgressOperation::Verify);
    }

    /// Signal that the programming procedure completed successfully.
    pub(super) fn finished_programming(&self) {
        self.finished(ProgressOperation::Program);
    }

    /// Signal that the erasing procedure completed successfully.
    pub(super) fn finished_erasing(&self) {
        self.finished(ProgressOperation::Erase);
    }

    /// Signal that the filling procedure completed successfully.
    pub(super) fn finished_filling(&self) {
        self.finished(ProgressOperation::Fill);
    }

    /// Signal that the verifying procedure completed successfully.
    pub(super) fn finished_verifying(&self) {
        self.finished(ProgressOperation::Verify);
    }
}

/// The operation that is currently in progress.
#[derive(Clone, Copy, Debug)]
pub enum ProgressOperation {
    /// Reading back flash contents to restore erased regions that should be kept unchanged.
    Fill,

    /// Erasing flash sectors.
    Erase,

    /// Writing data to flash.
    Program,

    /// Checking flash contents.
    Verify,
}

/// Possible events during the flashing process.
///
/// If flashing works without problems, the events will arrive in the
/// following order:
///
/// * `FlashLayoutReady`
/// * A number of `AddProgressBar` events
/// * `Started`, `Progress`, and `Finished` events for each operation
///
/// If an error occurs in any stage, the `Failed` event will be returned,
/// and no further events will be returned.
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// The flash layout is ready.
    FlashLayoutReady {
        /// The flash layout.
        flash_layout: Vec<FlashLayout>,
    },

    /// Display a new progress bar to the user.
    AddProgressBar {
        /// The operation that the progress bar is for.
        operation: ProgressOperation,
        /// The total size of the operation, if known.
        ///
        /// If `None`, the total size is indeterminate.
        total: Option<u64>,
    },

    /// Started an operation with the given total size.
    Started(ProgressOperation),

    /// An operation has made progress.
    Progress {
        /// The operation that made progress.
        operation: ProgressOperation,
        /// The size of the page in bytes.
        size: u64,
        /// The time it took to perform the operation.
        time: Duration,
    },

    /// An operation has failed.
    Failed(ProgressOperation),

    /// An operation has finished successfully.
    Finished(ProgressOperation),

    /// A message was received from the algo.
    DiagnosticMessage {
        /// The message that was emitted.
        message: String,
    },
}
