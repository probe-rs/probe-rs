use super::FlashLayout;
use std::time::Duration;

impl ProgressEvent {
    /// Signal that the flashing algorithm was set up and is initialized.
    pub(super) fn initialized(phases: Vec<FlashLayout>) -> Self {
        ProgressEvent::FlashLayoutReady {
            flash_layout: phases,
        }
    }

    /// Signal that a new progress bar should be created.
    pub(super) fn add_progress_bar(operation: ProgressOperation, total: Option<u64>) -> Self {
        ProgressEvent::AddProgressBar { operation, total }
    }

    /// Signal that the procedure started.
    pub(super) fn started(operation: ProgressOperation) -> Self {
        ProgressEvent::Started(operation)
    }

    /// Signal that the procedure has made progress.
    pub(super) fn progressed(operation: ProgressOperation, size: u64, time: Duration) -> Self {
        ProgressEvent::Progress {
            operation,
            size,
            time,
        }
    }

    /// Signal that the procedure failed.
    pub(super) fn failed(operation: ProgressOperation) -> Self {
        ProgressEvent::Failed(operation)
    }

    /// Signal that the procedure completed successfully.
    pub(super) fn finished(operation: ProgressOperation) -> Self {
        ProgressEvent::Finished(operation)
    }

    /// Signal that a flashing algorithm produced a diagnostic message.
    pub(super) fn message(message: String) -> Self {
        ProgressEvent::DiagnosticMessage { message }
    }

    // --- Methods for emitting events for a specific operation.

    /// Signal that the erasing procedure started.
    pub(super) fn started_erasing() -> Self {
        Self::started(ProgressOperation::Erase)
    }

    /// Signal that the filling procedure started.
    pub(super) fn started_filling() -> Self {
        Self::started(ProgressOperation::Fill)
    }

    /// Signal that the programming procedure started.
    pub(super) fn started_programming() -> Self {
        Self::started(ProgressOperation::Program)
    }

    /// Signal that the verifying procedure started.
    pub(crate) fn started_verifying() -> Self {
        Self::started(ProgressOperation::Verify)
    }

    /// Signal that the sector erasing procedure has made progress.
    pub(super) fn sector_erased(size: u64, time: Duration) -> Self {
        Self::progressed(ProgressOperation::Erase, size, time)
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) fn page_filled(size: u64, time: Duration) -> Self {
        Self::progressed(ProgressOperation::Fill, size, time)
    }

    /// Signal that the page programming procedure has made progress.
    pub(super) fn page_programmed(size: u64, time: Duration) -> Self {
        Self::progressed(ProgressOperation::Program, size, time)
    }

    /// Signal that the page filling procedure has made progress.
    pub(super) fn page_verified(size: u64, time: Duration) -> Self {
        Self::progressed(ProgressOperation::Verify, size, time)
    }

    /// Signal that the erasing procedure failed.
    pub(super) fn failed_erasing() -> Self {
        Self::failed(ProgressOperation::Erase)
    }

    /// Signal that the filling procedure failed.
    pub(super) fn failed_filling() -> Self {
        Self::failed(ProgressOperation::Fill)
    }

    /// Signal that the programming procedure failed.
    pub(super) fn failed_programming() -> Self {
        Self::failed(ProgressOperation::Program)
    }

    /// Signal that the verifying procedure failed.
    pub(super) fn failed_verifying() -> Self {
        Self::failed(ProgressOperation::Verify)
    }

    /// Signal that the programming procedure completed successfully.
    pub(super) fn finished_programming() -> Self {
        Self::finished(ProgressOperation::Program)
    }

    /// Signal that the erasing procedure completed successfully.
    pub(super) fn finished_erasing() -> Self {
        Self::finished(ProgressOperation::Erase)
    }

    /// Signal that the filling procedure completed successfully.
    pub(super) fn finished_filling() -> Self {
        Self::finished(ProgressOperation::Fill)
    }

    /// Signal that the verifying procedure completed successfully.
    pub(super) fn finished_verifying() -> Self {
        Self::finished(ProgressOperation::Verify)
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
