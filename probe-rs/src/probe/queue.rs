use core::fmt;
use std::{collections::HashMap, marker::PhantomData, sync::Arc};

use crate::probe::{CommandResult, DebugProbeError, JtagCommand};

/// Internal, batch specific, error.
///
/// This is generic over the error type `E`, with a default of a boxed error
/// for use in the internal JTAG layer. When using [`JtagQueue<E>`], the error
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
/// When using `JtagQueue<E>`, you get back `BatchExecutionError<E>` with
/// your concrete error type.
#[derive(thiserror::Error, Debug)]
pub struct BatchExecutionError<E = Box<dyn std::error::Error + Send + Sync>> {
    /// The error that occurred during execution.
    #[source]
    pub error: BatchError<E>,

    /// The results of the commands that were executed before the error occurred.
    pub results: Results,
}

impl BatchExecutionError {
    pub(crate) fn new_specific(
        error: Box<dyn std::error::Error + Send + Sync>,
        results: Results,
    ) -> Self {
        BatchExecutionError {
            // Just in case the caller passed a boxed DebugProbeError, which they weren't supposed to, convert it back.
            error: match error.downcast::<DebugProbeError>() {
                Ok(error) => BatchError::Probe(*error),
                Err(error) => BatchError::Specific(error),
            },
            results,
        }
    }

    pub(crate) fn new_from_debug_probe(error: DebugProbeError, results: Results) -> Self {
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

/// Batch for probe commands with compile-time error type enforcement.
///
/// All commands scheduled in this batch must use the same error type `E`.
#[derive(Debug)]
pub struct Batch<Op, E> {
    batch: ErasedBatch<Op>,
    _marker: PhantomData<E>,
}

/// A JTAG command batch with compile-time error type enforcement.
pub type JtagQueue<E> = Batch<JtagCommand, E>;

impl<Op, E: std::error::Error + Send + Sync + 'static> Default for Batch<Op, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Op, E: std::error::Error + Send + Sync + 'static> Batch<Op, E> {
    /// Returns whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.batch.is_empty()
    }

    /// Returns the number of commands in the batch.
    pub fn len(&self) -> usize {
        self.batch.len()
    }

    /// Skip successfully executed commands for partial retry.
    pub fn consume(&mut self, len: usize) {
        self.batch.consume(len)
    }

    /// Rewind to re-execute commands for retry scenarios.
    ///
    /// Returns `true` if successful, `false` if more commands were requested than available.
    pub fn rewind(&mut self, by: usize) -> bool {
        self.batch.rewind(by)
    }
}

impl<Op, E: std::error::Error + Send + Sync + 'static> Batch<Op, E> {
    /// Creates a new empty typed batch.
    pub fn new() -> Self {
        Self {
            batch: ErasedBatch::new(),
            _marker: PhantomData,
        }
    }

    /// Schedule a command.
    ///
    /// The error type is erased internally, but will be recovered when
    /// `execute()` returns an error.
    pub fn schedule(&mut self, cmd: impl Into<Op>) -> Handle<CommandResult> {
        self.batch.schedule(cmd)
    }

    /// Execute the batch and return results with typed errors.
    ///
    /// # Errors
    ///
    /// Returns `BatchExecutionError<E>` if any command fails. The error
    /// contains the concrete error type `E`, not a boxed trait object.
    pub fn execute<F>(&self, command: F) -> Result<Results, BatchExecutionError<E>>
    where
        F: FnOnce(&ErasedBatch<Op>) -> Result<Results, BatchExecutionError>,
    {
        command(&self.batch).map_err(|e| e.downcast_specific::<E>())
    }
}

/// The set of results returned by executing a batched command.
#[derive(Default)]
pub struct Results(HashMap<HandleId, CommandResult>);

impl fmt::Debug for Results {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Results").field(&self.0).finish()
    }
}

impl Results {
    /// Creates a new empty result set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new empty result set with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashMap::with_capacity(capacity))
    }

    pub(crate) fn push(&mut self, id: &HandleId, result: CommandResult) {
        self.0.insert(id.clone(), result);
    }

    /// Returns the number of results in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn merge_from(&mut self, other: Results) {
        self.0.extend(other.0);
        self.0.retain(|k, _| k.should_capture());
    }

    /// Takes a result from the set.
    pub fn take<T>(&mut self, handle: Handle<T>) -> Result<T, Handle<T>> {
        let Handle { id, convert } = handle;
        match self.0.remove(&id) {
            Some(result) => Ok(convert(result)),
            None => Err(Handle { id, convert }),
        }
    }
}

/// Identifier for a scheduled command result.
///
/// The batch stores a clone of this value. The caller holds a [`Handle`].
#[derive(Eq)]
pub struct HandleId(Arc<()>);

impl PartialEq for HandleId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for HandleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HandleId")
            .field(&(Arc::as_ptr(&self.0) as usize))
            .finish()
    }
}

impl HandleId {
    pub(crate) fn new() -> Self {
        Self(Arc::new(()))
    }

    pub(crate) fn should_capture(&self) -> bool {
        // Both the batch and the user code may hold on to at most one of the references. The batch
        // execution will be able to detect if the user dropped their read reference, meaning
        // the read data would be inaccessible.
        Arc::strong_count(&self.0) > 1
    }
}

impl Clone for HandleId {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl std::hash::Hash for HandleId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state)
    }
}

/// A handle used to retrieve the result of a scheduled command.
///
/// This type can detect if the result of a command is not used.
pub struct Handle<T> {
    id: HandleId,
    convert: Box<dyn FnOnce(CommandResult) -> T + Send>,
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for Handle<T> {}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Handle<CommandResult> {
    pub(crate) fn from_id(id: HandleId) -> Self {
        Self {
            id,
            convert: Box::new(|result| result),
        }
    }

    pub(crate) fn id(&self) -> &HandleId {
        &self.id
    }
}

impl<T: 'static> Handle<T> {
    /// Maps the result when the caller redeems the handle.
    ///
    /// The mapping does not touch the probe. It cannot fail.
    pub fn map<U>(self, f: impl FnOnce(T) -> U + Send + 'static) -> Handle<U> {
        let id = self.id;
        let convert = self.convert;
        Handle {
            id,
            convert: Box::new(move |result| f(convert(result))),
        }
    }
}

/// A set of batched commands that will be processed in a batch by the probe.
///
/// If possible, the [`Batch`] should be used which
///
/// This list maintains which commands' results can be read by the issuing code, which then
/// can be used to skip capturing or processing certain parts of the response.
#[derive(Debug)]
pub struct ErasedBatch<Op> {
    commands: Vec<(HandleId, Op)>,
    cursor: usize,
}

impl<Op> ErasedBatch<Op> {
    /// Creates a new empty batch.
    fn new() -> Self {
        Self {
            commands: Vec::new(),
            cursor: 0,
        }
    }

    /// Schedules a command for later execution.
    ///
    /// Returns a token value that can be used to retrieve the result of the command.
    fn schedule(&mut self, command: impl Into<Op>) -> Handle<CommandResult> {
        let id = HandleId::new();
        self.commands.push((id.clone(), command.into()));
        Handle::from_id(id)
    }

    /// Returns the number of commands in the batch.
    pub fn len(&self) -> usize {
        self.commands[self.cursor..].len()
    }

    /// Returns whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &(HandleId, Op)> {
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

/// Deprecated. Use [`Batch`] instead.
#[deprecated(note = "renamed to Batch")]
pub type Queue<E> = Batch<JtagCommand, E>;

/// Deprecated. Use [`ErasedBatch`] instead.
#[deprecated(note = "renamed to ErasedBatch")]
pub type ErasedQueue = ErasedBatch<JtagCommand>;

/// Deprecated. Use [`Handle`] instead.
#[deprecated(note = "renamed to Handle")]
pub type DeferredResultIndex = Handle<CommandResult>;

/// Deprecated. Use [`Results`] instead.
#[deprecated(note = "renamed to Results")]
pub type DeferredResultSet = Results;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, thiserror::Error)]
    #[error("test error")]
    struct TestError;

    #[derive(Debug, Clone, Copy)]
    enum TestOp {
        A,
        B,
        C,
        FailTransform,
    }

    fn run_batch<F>(
        batch: &Batch<TestOp, TestError>,
        execute: F,
    ) -> Result<Results, BatchExecutionError<TestError>>
    where
        F: FnOnce(&ErasedBatch<TestOp>) -> Result<Results, BatchExecutionError>,
    {
        batch.execute(execute)
    }

    fn execute_success(batch: &ErasedBatch<TestOp>) -> Result<Results, BatchExecutionError> {
        let mut results = Results::new();
        for (id, op) in batch.iter() {
            let result = match op {
                TestOp::A => CommandResult::U32(1),
                TestOp::B => CommandResult::U32(2),
                TestOp::C => CommandResult::U32(3),
                TestOp::FailTransform => CommandResult::U32(0),
            };
            if id.should_capture() {
                results.push(id, result);
            }
        }
        Ok(results)
    }

    fn execute_fail_at_second(batch: &ErasedBatch<TestOp>) -> Result<Results, BatchExecutionError> {
        let mut results = Results::new();
        for (i, (id, op)) in batch.iter().enumerate() {
            if i == 1 {
                return Err(BatchExecutionError::new_specific(
                    Box::new(TestError),
                    results,
                ));
            }
            if id.should_capture() {
                results.push(
                    id,
                    match op {
                        TestOp::A => CommandResult::U32(1),
                        _ => CommandResult::None,
                    },
                );
            }
        }
        Ok(results)
    }

    fn execute_fail_transform(batch: &ErasedBatch<TestOp>) -> Result<Results, BatchExecutionError> {
        let mut results = Results::new();
        for (id, op) in batch.iter() {
            if matches!(op, TestOp::FailTransform) {
                return Err(BatchExecutionError::new_specific(
                    Box::new(TestError),
                    results,
                ));
            }
            if id.should_capture() {
                results.push(id, CommandResult::U32(1));
            }
        }
        Ok(results)
    }

    #[test]
    fn three_operations_return_three_results_in_order() {
        let mut batch = Batch::<TestOp, TestError>::new();
        let h0 = batch.schedule(TestOp::A);
        let h1 = batch.schedule(TestOp::B);
        let h2 = batch.schedule(TestOp::C);

        let mut results = run_batch(&batch, execute_success).unwrap();

        assert!(matches!(results.take(h0).unwrap(), CommandResult::U32(1)));
        assert!(matches!(results.take(h1).unwrap(), CommandResult::U32(2)));
        assert!(matches!(results.take(h2).unwrap(), CommandResult::U32(3)));
    }

    #[test]
    fn dropped_handle_is_not_captured() {
        let mut batch = Batch::<TestOp, TestError>::new();
        batch.schedule(TestOp::A);
        let _held = batch.schedule(TestOp::B);

        run_batch(&batch, |erased| {
            let capture: Vec<_> = erased.iter().map(|(id, _)| id.should_capture()).collect();
            assert_eq!(capture, [false, true]);
            Ok(Results::new())
        })
        .unwrap();
    }

    #[test]
    fn map_runs_once_at_take() {
        let mut batch = Batch::<TestOp, TestError>::new();
        let count = Arc::new(AtomicUsize::new(0));
        let count_in_map = Arc::clone(&count);
        let handle = batch.schedule(TestOp::A).map(move |r| {
            count_in_map.fetch_add(1, Ordering::SeqCst);
            r
        });

        let mut results = run_batch(&batch, execute_success).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(matches!(
            results.take(handle).unwrap(),
            CommandResult::U32(1)
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn chained_maps_run_in_order() {
        let mut batch = Batch::<TestOp, TestError>::new();
        let handle = batch
            .schedule(TestOp::A)
            .map(|r| match r {
                CommandResult::U32(v) => v + 1,
                _ => 0,
            })
            .map(|v| v * 10);

        let mut results = run_batch(&batch, execute_success).unwrap();
        assert_eq!(results.take(handle).unwrap(), 20);
    }

    #[test]
    fn take_with_handle_from_other_batch_returns_handle() {
        let mut batch_a = Batch::<TestOp, TestError>::new();
        let mut batch_b = Batch::<TestOp, TestError>::new();
        let handle_a = batch_a.schedule(TestOp::A);
        let _handle_b = batch_b.schedule(TestOp::B);

        let mut results = run_batch(&batch_b, execute_success).unwrap();
        assert!(results.take(handle_a).is_err());
    }

    #[test]
    fn failed_batch_returns_results_before_fault_and_fault_index() {
        let mut batch = Batch::<TestOp, TestError>::new();
        let h0 = batch.schedule(TestOp::A);
        let _h1 = batch.schedule(TestOp::B);
        let _h2 = batch.schedule(TestOp::C);

        let mut err = run_batch(&batch, execute_fail_at_second).unwrap_err();
        assert_eq!(err.results.len(), 1);
        assert!(matches!(
            err.results.take(h0).unwrap(),
            CommandResult::U32(1)
        ));
    }

    #[test]
    fn failing_probe_transform_aborts_batch() {
        let mut batch = Batch::<TestOp, TestError>::new();
        let h0 = batch.schedule(TestOp::A);
        let _h1 = batch.schedule(TestOp::FailTransform);
        let _h2 = batch.schedule(TestOp::C);

        let mut err = run_batch(&batch, execute_fail_transform).unwrap_err();
        assert_eq!(err.results.len(), 1);
        assert!(matches!(
            err.results.take(h0).unwrap(),
            CommandResult::U32(1)
        ));
    }
}
