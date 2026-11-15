use core::fmt;
use rustc_hash::FxBuildHasher;
use std::{
    cell::RefCell,
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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

    /// Index of the operation that failed in the batch slice that was executed.
    pub fault_operation: usize,
}

impl<E> BatchExecutionError<E> {
    /// Report a probe error that failed the operation after the captured results.
    pub fn new_from_debug_probe(error: DebugProbeError, results: Results) -> Self {
        let fault_operation = results.len();
        Self::new_from_debug_probe_at(error, results, fault_operation)
    }

    /// Report a probe error that failed the operation at the given index of the batch.
    pub fn new_from_debug_probe_at(
        error: DebugProbeError,
        results: Results,
        fault_operation: usize,
    ) -> Self {
        BatchExecutionError {
            error: BatchError::Probe(error),
            results,
            fault_operation,
        }
    }
}

impl BatchExecutionError {
    pub(crate) fn new_specific(
        error: Box<dyn std::error::Error + Send + Sync>,
        results: Results,
    ) -> Self {
        let fault_operation = results.len();
        BatchExecutionError {
            // Just in case the caller passed a boxed DebugProbeError, which they weren't supposed to, convert it back.
            error: match error.downcast::<DebugProbeError>() {
                Ok(error) => BatchError::Probe(*error),
                Err(error) => BatchError::Specific(error),
            },
            results,
            fault_operation,
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
            fault_operation: self.fault_operation,
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

    /// Schedule a command under the ID of an earlier command. The handle of that ID gets the
    /// result.
    pub(crate) fn schedule_preserved(&mut self, id: HandleId, cmd: impl Into<Op>) {
        self.batch.schedule_preserved(id, cmd)
    }

    pub(crate) fn replace_remaining(&mut self, ops: Vec<(HandleId, Op)>) {
        self.batch.replace_remaining(ops);
    }

    pub(crate) fn remaining_with_ids(&self) -> Vec<(HandleId, Op)>
    where
        Op: Clone,
    {
        self.batch.remaining_with_ids()
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

    /// Iterate over the operations in the batch, in the order of the schedule.
    ///
    /// A probe driver walks the batch with this method.
    pub fn iter(&self) -> impl Iterator<Item = &(HandleId, Op)> {
        self.batch.iter()
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
pub struct Results(HashMap<HandleId, CommandResult, FxBuildHasher>);

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
        Self(HashMap::with_capacity_and_hasher(capacity, FxBuildHasher))
    }

    /// Record the result of one operation.
    ///
    /// A probe driver reports the answer of an operation with this method.
    pub fn push(&mut self, id: &HandleId, result: CommandResult) {
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
        // Take the first set over rather than inserting it again.
        if self.0.is_empty() {
            self.0 = other.0;
        } else {
            self.0.extend(other.0);
        }
        self.0.retain(|k, _| k.should_capture());
    }

    /// Takes the result stored under `id`, unconverted.
    pub(crate) fn take_id(&mut self, id: &HandleId) -> Option<CommandResult> {
        self.0.remove(id)
    }

    /// Takes a result from the set.
    pub fn take<T>(&mut self, mut handle: Handle<T>) -> Result<T, Handle<T>> {
        match self.0.remove(&handle.id) {
            Some(result) => Ok(handle.take_convert()(result)),
            None => Err(handle),
        }
    }
}

const HANDLE_BLOCK_SLOTS: usize = 1024;

/// Whether the handle of each of [`HANDLE_BLOCK_SLOTS`] IDs was abandoned.
///
/// The IDs of one batch share a block, so a driver that checks them reads neighbouring memory.
struct HandleBlock {
    abandoned: [AtomicBool; HANDLE_BLOCK_SLOTS],
}

impl HandleBlock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            abandoned: [const { AtomicBool::new(false) }; HANDLE_BLOCK_SLOTS],
        })
    }
}

thread_local! {
    /// The block that new IDs take their slot from, and the next free slot in it.
    static CURRENT_HANDLE_BLOCK: RefCell<(Arc<HandleBlock>, usize)> =
        RefCell::new((HandleBlock::new(), 0));
}

/// Identifier for a scheduled command result.
///
/// The batch stores a clone of this value. The caller holds a [`Handle`].
#[derive(Clone)]
pub struct HandleId {
    block: Arc<HandleBlock>,
    slot: usize,
}

impl PartialEq for HandleId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.block, &other.block) && self.slot == other.slot
    }
}

impl Eq for HandleId {}

impl fmt::Debug for HandleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("HandleId")
            .field(&(Arc::as_ptr(&self.block) as usize))
            .field(&self.slot)
            .finish()
    }
}

impl HandleId {
    /// Takes the next slot. Only [`Handle::new`] calls this, so each ID has exactly one handle.
    fn new() -> Self {
        let take_slot = |(block, next): &mut (Arc<HandleBlock>, usize)| {
            if *next == HANDLE_BLOCK_SLOTS {
                *block = HandleBlock::new();
                *next = 0;
            }
            let slot = *next;
            *next += 1;
            (block.clone(), slot)
        };
        // The thread-local is gone while its thread exits. Such an ID gets a block of its own.
        let (block, slot) = CURRENT_HANDLE_BLOCK
            .try_with(|current| take_slot(&mut current.borrow_mut()))
            .unwrap_or_else(|_| (HandleBlock::new(), 0));

        Self { block, slot }
    }

    /// Report whether the caller still holds the handle of this operation.
    ///
    /// A probe driver skips the capture of an operation whose handle is gone,
    /// because nobody can read the answer.
    pub fn should_capture(&self) -> bool {
        !self.abandoned().load(Ordering::Relaxed)
    }

    fn abandoned(&self) -> &AtomicBool {
        &self.block.abandoned[self.slot]
    }
}

impl std::hash::Hash for HandleId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.block) as usize).hash(state);
        self.slot.hash(state);
    }
}

/// A handle used to retrieve the result of a scheduled command.
///
/// This type can detect if the result of a command is not used. Each ID has exactly one handle.
/// When the handle drops before [`Results::take`], the operations of its ID stop to capture.
pub struct Handle<T> {
    id: HandleId,
    /// `None` after the handle gave its conversion away, so that the drop does not abandon the ID.
    convert: Option<Box<dyn FnOnce(CommandResult) -> T + Send>>,
}

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        if self.convert.is_some() {
            self.id.abandoned().store(true, Ordering::Relaxed);
        }
    }
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

impl<T> Handle<T> {
    pub(crate) fn id(&self) -> &HandleId {
        &self.id
    }

    /// Creates a handle with a new ID. The operations of the ID capture until the handle drops.
    pub(crate) fn new(convert: Box<dyn FnOnce(CommandResult) -> T + Send>) -> Self {
        Self {
            id: HandleId::new(),
            convert: Some(convert),
        }
    }

    fn take_convert(&mut self) -> Box<dyn FnOnce(CommandResult) -> T + Send> {
        self.convert
            .take()
            .expect("a handle holds its conversion until it gives it away")
    }
}

impl Handle<CommandResult> {
    /// Creates a handle with a new ID that returns the result as it is.
    pub(crate) fn unconverted() -> Self {
        Self::new(Box::new(|result| result))
    }
}

impl<T: 'static> Handle<T> {
    /// Maps the result when the caller redeems the handle.
    ///
    /// The mapping does not touch the probe. It cannot fail.
    pub fn map<U>(mut self, f: impl FnOnce(T) -> U + Send + 'static) -> Handle<U> {
        let convert = self.take_convert();
        Handle {
            id: self.id.clone(),
            convert: Some(Box::new(move |result| f(convert(result)))),
        }
    }

    /// Replaces the conversion of the result. The ID stays in use.
    pub(crate) fn with_convert<U>(
        mut self,
        convert: Box<dyn FnOnce(CommandResult) -> U + Send>,
    ) -> Handle<U> {
        drop(self.take_convert());
        Handle {
            id: self.id.clone(),
            convert: Some(convert),
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

impl<Op: Clone> ErasedBatch<Op> {
    fn remaining_with_ids(&self) -> Vec<(HandleId, Op)> {
        self.commands[self.cursor..].to_vec()
    }
}

impl<Op> ErasedBatch<Op> {
    fn replace_remaining(&mut self, ops: Vec<(HandleId, Op)>) {
        self.commands.truncate(self.cursor);
        self.commands.extend(ops);
    }
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
        let handle = Handle::unconverted();
        self.commands.push((handle.id().clone(), command.into()));
        handle
    }

    fn schedule_preserved(&mut self, id: HandleId, command: impl Into<Op>) {
        self.commands.push((id, command.into()));
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
