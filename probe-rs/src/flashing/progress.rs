use tokio::sync::mpsc::Sender;

use super::FlashLayout;
use std::time::Duration;

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
    sender: Option<Sender<ProgressEvent>>,
}

impl FlashProgress {
    /// Create a new `FlashProgress` structure with a given `handler` to be called on events.
    pub fn new(sender: Sender<ProgressEvent>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    /// Create a new `FlashProgress` structure with an empty handler.
    pub fn empty() -> Self {
        Self { sender: None }
    }

    /// Emit a flashing progress event.
    pub async fn emit(&self, event: ProgressEvent) {
        if let Some(sender) = &self.sender {
            sender.send(event).await.unwrap()
        }
    }

    // --- Methods for emitting specific kinds of events.

    /// Signal that the flashing algorithm was set up and is initialized.
    pub(super) async fn initialized(&self, phases: Vec<FlashLayout>) {
        self.emit(ProgressEvent::FlashLayoutReady {
            flash_layout: phases,
        })
        .await;
    }

    /// Signal that a new progress bar should be created.
    pub(super) async fn add_progress_bar(&self, operation: ProgressOperation, total: Option<u64>) {
        self.emit(ProgressEvent::AddProgressBar { operation, total })
            .await;
    }

    /// Signal that the procedure started.
    pub(super) async fn started(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Started(operation)).await;
    }

    /// Signal that the procedure has made progress.
    pub(super) async fn progressed(&self, operation: ProgressOperation, size: u64, time: Duration) {
        self.emit(ProgressEvent::Progress {
            operation,
            size,
            time,
        })
        .await;
    }

    /// Signal that the procedure failed.
    pub(super) async fn failed(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Failed(operation)).await;
    }

    /// Signal that the procedure completed successfully.
    pub(super) async fn finished(&self, operation: ProgressOperation) {
        self.emit(ProgressEvent::Finished(operation)).await;
    }

    /// Signal that a flashing algorithm produced a diagnostic message.
    pub(super) async fn message(&self, message: String) {
        self.emit(ProgressEvent::DiagnosticMessage { message })
            .await;
    }

    // --- Methods for emitting events for a specific operation.

    /// Signal that the erasing procedure started.
    pub(super) async fn started_erasing(&self) {
        self.started(ProgressOperation::Erase).await;
    }

    /// Signal that the filling procedure started.
    pub(super) async fn started_filling(&self) {
        self.started(ProgressOperation::Fill).await;
    }

    /// Signal that the programming procedure started.
    pub(super) async fn started_programming(&self) {
        self.started(ProgressOperation::Program).await;
    }

    /// Signal that the verifying procedure started.
    pub(crate) async fn started_verifying(&self) {
        self.started(ProgressOperation::Verify).await;
    }

    /// Signal that the sector erasing procedure has made progress.
    pub(super) async fn sector_erased(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Erase, size, time).await;
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) async fn page_filled(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Fill, size, time).await;
    }

    /// Signal that the page programming procedure has made progress.
    pub(super) async fn page_programmed(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Program, size, time)
            .await;
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) async fn page_verified(&self, size: u64, time: Duration) {
        self.progressed(ProgressOperation::Verify, size, time).await;
    }

    /// Signal that the erasing procedure failed.
    pub(super) async fn failed_erasing(&self) {
        self.failed(ProgressOperation::Erase).await;
    }

    /// Signal that the filling procedure failed.
    pub(super) async fn failed_filling(&self) {
        self.failed(ProgressOperation::Fill).await;
    }

    /// Signal that the programming procedure failed.
    pub(super) async fn failed_programming(&self) {
        self.failed(ProgressOperation::Program).await;
    }

    /// Signal that the verifying procedure failed.
    pub(super) async fn failed_verifying(&self) {
        self.failed(ProgressOperation::Verify).await;
    }

    /// Signal that the programming procedure completed successfully.
    pub(super) async fn finished_programming(&self) {
        self.finished(ProgressOperation::Program).await;
    }

    /// Signal that the erasing procedure completed successfully.
    pub(super) async fn finished_erasing(&self) {
        self.finished(ProgressOperation::Erase).await;
    }

    /// Signal that the filling procedure completed successfully.
    pub(super) async fn finished_filling(&self) {
        self.finished(ProgressOperation::Fill).await;
    }

    /// Signal that the verifying procedure completed successfully.
    pub(super) async fn finished_verifying(&self) {
        self.finished(ProgressOperation::Verify).await;
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
