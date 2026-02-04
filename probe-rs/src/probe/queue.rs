use core::fmt;
use std::{collections::HashMap, sync::Arc};

use crate::probe::{CommandResult, DebugProbeError, JtagCommand};

/// Internal, batch specific, error.
///
/// This is generic over the error type `E`, with a default of a boxed error
/// for use in the internal JTAG layer. When using [`Queue<E>`], the error
/// can be downcast back to the concrete type.
#[derive(Debug, thiserror::Error)]
pub enum BatchError<E> {
    /// Batch error specific to a debug interface occurred
    #[error(transparent)]
    Specific(E),
    /// The probe encountered an error while processing the batch
    #[error(transparent)]
    Probe(DebugProbeError),
}

/// An error that occurred during batched command execution of JTAG commands.
///
/// This is generic over the error type `E`, with a default of a boxed error.
/// When using `Queue<E>`, you get back `BatchExecutionError<E>` with
/// your concrete error type.
#[derive(thiserror::Error, Debug)]
pub struct BatchExecutionError<E = Box<dyn std::error::Error + Send + Sync>> {
    /// The error that occurred during execution.
    #[source]
    pub error: BatchError<E>,

    /// The results of the commands that were executed before the error occurred.
    pub results: DeferredResultSet<CommandResult>,
}

impl BatchExecutionError {
    pub(crate) fn new_specific(
        error: Box<dyn std::error::Error + Send + Sync>,
        results: DeferredResultSet<CommandResult>,
    ) -> Self {
        BatchExecutionError {
            error: BatchError::Specific(error),
            results,
        }
    }

    pub(crate) fn new_from_debug_probe(
        error: DebugProbeError,
        results: DeferredResultSet<CommandResult>,
    ) -> Self {
        BatchExecutionError {
            error: BatchError::Probe(error),
            results,
        }
    }

    /// Downcast the boxed error to a concrete type.
    ///
    /// # Panics
    ///
    /// Panics if the error is not of type `E`. This should only be used
    /// when you know all commands in the batch use the same error type.
    pub fn downcast_specific<T>(self) -> BatchExecutionError<T>
    where
        T: std::error::Error + Send + Sync + 'static,
    {
        BatchExecutionError {
            error: match self.error {
                BatchError::Specific(boxed) => BatchError::Specific(
                    *boxed
                        .downcast::<T>()
                        .expect("error type mismatch in downcast_specific"),
                ),
                BatchError::Probe(e) => BatchError::Probe(e),
            },
            results: self.results,
        }
    }
}

impl<E: std::fmt::Display> std::fmt::Display for BatchExecutionError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Error cause was {}. Successful command count {}",
            self.error,
            self.results.len()
        )
    }
}
/// Queue for  JTAG commands with compile-time error type enforcement.
///
/// All commands scheduled in this queue must use the same error type `E`.
///
#[derive(Debug)]
pub struct Queue<E> {
    queue: ErasedQueue,
    _marker: std::marker::PhantomData<E>,
}

impl<E: std::error::Error + Send + Sync + 'static> Default for Queue<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: std::error::Error + Send + Sync + 'static> Queue<E> {
    /// Creates a new empty typed batch.
    pub fn new() -> Self {
        Self {
            queue: ErasedQueue::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Schedule a command.
    ///
    /// The error type is erased internally, but will be recovered when
    /// `execute()` returns an error.
    pub fn schedule(&mut self, cmd: impl Into<JtagCommand>) -> DeferredResultIndex {
        self.queue.schedule(cmd)
    }
}

impl<E> Queue<E> {
    /// Returns whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns the number of commands in the batch.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Skip successfully executed commands for partial retry.
    pub fn consume(&mut self, len: usize) {
        self.queue.consume(len)
    }

    /// Rewind to re-execute commands for retry scenarios.
    ///
    /// Returns `true` if successful, `false` if more commands were requested than available.
    pub fn rewind(&mut self, by: usize) -> bool {
        self.queue.rewind(by)
    }
}

impl<E: std::error::Error + Send + Sync + 'static> Queue<E> {
    /// Execute the batch and return results with typed errors.
    ///
    /// # Errors
    ///
    /// Returns `BatchExecutionError<E>` if any command fails. The error
    /// contains the concrete error type `E`, not a boxed trait object.
    pub fn execute<F>(
        &self,
        command: F,
    ) -> Result<DeferredResultSet<CommandResult>, BatchExecutionError<E>>
    where
        F: FnOnce(&ErasedQueue) -> Result<DeferredResultSet<CommandResult>, BatchExecutionError>,
    {
        command(&self.queue).map_err(|e| e.downcast_specific::<E>())
    }
}

/// The set of results returned by executing a batched command.
pub struct DeferredResultSet<T>(HashMap<DeferredResultIndex, T>);

impl<T: std::fmt::Debug> std::fmt::Debug for DeferredResultSet<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DeferredResultSet").field(&self.0).finish()
    }
}

impl<T> Default for DeferredResultSet<T> {
    fn default() -> Self {
        Self(HashMap::default())
    }
}

impl<T> DeferredResultSet<T> {
    /// Creates a new empty result set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new empty result set with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashMap::with_capacity(capacity))
    }

    pub(crate) fn push(&mut self, idx: &DeferredResultIndex, result: T) {
        self.0.insert(idx.clone(), result);
    }

    /// Returns the number of results in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn merge_from(&mut self, other: DeferredResultSet<T>) {
        self.0.extend(other.0);
        self.0.retain(|k, _| k.should_capture());
    }

    /// Takes a result from the set.
    pub fn take(&mut self, index: DeferredResultIndex) -> Result<T, DeferredResultIndex> {
        self.0.remove(&index).ok_or(index)
    }
}

/// An index type used to retrieve the result of a deferred command.
///
/// This type can detect if the result of a command is not used.
#[derive(Eq)]
pub struct DeferredResultIndex(Arc<()>);

impl PartialEq for DeferredResultIndex {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for DeferredResultIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DeferredResultIndex")
            .field(&self.id())
            .finish()
    }
}

impl DeferredResultIndex {
    // Intentionally private. User code must not be able to create these.
    fn new() -> Self {
        Self(Arc::new(()))
    }

    fn id(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub(crate) fn should_capture(&self) -> bool {
        // Both the queue and the user code may hold on to at most one of the references. The queue
        // execution will be able to detect if the user dropped their read reference, meaning
        // the read data would be inaccessible.
        Arc::strong_count(&self.0) > 1
    }

    // Intentionally private. User code must not be able to clone these.
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::hash::Hash for DeferredResultIndex {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state)
    }
}

/// A set of batched JTAG commands that will be processed in a batch by the probe.
///
/// If possible, the [`Queue`] should be used which
///
/// This list maintains which commands' results can be read by the issuing code, which then
/// can be used to skip capturing or processing certain parts of the response.
#[derive(Debug, Default)]
pub struct ErasedQueue {
    commands: Vec<(DeferredResultIndex, JtagCommand)>,
    cursor: usize,
}

impl ErasedQueue {
    /// Creates a new empty queue.
    fn new() -> Self {
        Self::default()
    }

    /// Schedules a command for later execution.
    ///
    /// Returns a token value that can be used to retrieve the result of the command.
    fn schedule(&mut self, command: impl Into<JtagCommand>) -> DeferredResultIndex {
        let index = DeferredResultIndex::new();
        self.commands.push((index.clone(), command.into()));
        index
    }

    /// Returns the number of commands in the queue.
    pub fn len(&self) -> usize {
        self.commands[self.cursor..].len()
    }

    /// Returns whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &(DeferredResultIndex, JtagCommand)> {
        self.commands[self.cursor..].iter()
    }

    /// Rewinds the cursor by the specified number of commands.
    ///
    /// Returns `true` if the cursor was successfully rewound, `false` if more commands were requested than available.
    pub(crate) fn rewind(&mut self, by: usize) -> bool {
        if self.cursor >= by {
            self.cursor -= by;
            true
        } else {
            false
        }
    }

    /// Removes the first `len` number of commands from the batch.
    pub(crate) fn consume(&mut self, len: usize) {
        debug_assert!(self.len() >= len);
        self.cursor += len;
    }
}
