//! ADIv5 transaction rules for SWD probes.

use std::collections::HashSet;

use super::{Direction, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError};
use crate::probe::{
    BatchError, CommandResult, DebugProbeError, Handle, HandleId, Results, SwdSettings,
};

/// ABORT register address bits 2 and 3.
const DP_ABORT_ADDR: u8 = 0b0000;
/// CTRL/STAT register address bits 2 and 3.
const DP_CTRL_ADDR: u8 = 0b0100;
/// RDBUFF register address bits 2 and 3.
const DP_RDBUFF_ADDR: u8 = 0b1100;

/// ABORT value that clears overrun and sticky error bits.
const ABORT_CLEAR_STICKY: u32 = (1 << 4) | (1 << 2);
/// ABORT value that requests a DAP abort.
const ABORT_DAP_ABORT: u32 = 1;

/// Layer 1 SWD driver that owns ADIv5 transaction rules.
pub struct SwdPort<'p> {
    probe: &'p mut dyn SwdProbe,
    settings: SwdSettings,
    block_read_counts: Vec<usize>,
    block_read_handles: Vec<Vec<Handle<u32>>>,
}

/// An error from [`SwdPort`].
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum SwdPortError {
    /// The probe returned an SWD transfer error.
    Transfer(#[from] SwdTransferError),
    /// The probe returned an error.
    Probe(DebugProbeError),
}

impl PartialEq for SwdPortError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Transfer(left), Self::Transfer(right)) => left == right,
            _ => false,
        }
    }
}

impl<'p> SwdPort<'p> {
    /// Create a port over `probe` with `settings`.
    pub fn new(probe: &'p mut dyn SwdProbe, settings: SwdSettings) -> Self {
        Self {
            probe,
            settings,
            block_read_counts: Vec::new(),
            block_read_handles: Vec::new(),
        }
    }

    /// Schedule an AP block read on `batch`.
    ///
    /// The first acknowledgement carries no data. The caller drops that result.
    pub fn read_ap_block(
        &mut self,
        batch: &mut SwdBatch,
        addr: u8,
        count: usize,
    ) -> Handle<Vec<u32>> {
        if self.probe.handles_ap_pipeline() {
            let mut held = Vec::new();
            for _ in 1..count {
                held.push(batch.read(Port::Ap, addr));
            }
            let last = batch.read(Port::Ap, addr);

            self.block_read_counts.push(count);
            self.block_read_handles.push(held);

            return Handle::from_parts(last.id().clone(), Box::new(decode_block_read));
        }

        let _ = batch.read(Port::Ap, addr);

        if count <= 1 {
            return batch
                .read(Port::Dp, DP_RDBUFF_ADDR)
                .map(|value| vec![value]);
        }

        let mut held = Vec::new();
        for _ in 0..(count - 1) {
            held.push(batch.read(Port::Ap, addr));
        }
        let trailing = batch.read(Port::Dp, DP_RDBUFF_ADDR);

        self.block_read_counts.push(count);
        self.block_read_handles.push(held);

        Handle::from_parts(trailing.id().clone(), Box::new(decode_block_read))
    }

    /// Schedule an AP block write on `batch`.
    ///
    /// A trailing RDBUFF read verifies the posted writes.
    pub fn write_ap_block(&mut self, batch: &mut SwdBatch, addr: u8, values: &[u32]) {
        for &value in values {
            batch.write(Port::Ap, addr, value);
        }
        if !self.probe.handles_ap_pipeline() {
            let _ = batch.read(Port::Dp, DP_RDBUFF_ADDR);
        }
    }

    /// Expand the logical batch, execute it with WAIT retry, and return results.
    ///
    /// Other errors are not handled, so the debug interface might be in an error state
    /// after this function returns.
    pub fn run(&mut self, batch: SwdBatch) -> Result<Results, SwdPortError> {
        let ExpansionPlan {
            expanded,
            read_handles,
            logical,
            pipeline_handles,
        } = expand_batch(&batch, &self.settings, self.probe.handles_ap_pipeline());
        drop(batch);
        let mut expanded = expanded;
        let mut collected = Results::new();
        let mut idle_cycles = self.settings.num_idle_cycles_between_writes.max(1);
        let max_retries = self.settings.num_retries_after_wait;

        for _ in 0..max_retries {
            match self.probe.run_batch(&expanded) {
                Ok(results) => {
                    collected.merge_from(results);
                    let remapped =
                        remap_results(read_handles, logical, collected, &self.block_read_counts);
                    self.block_read_counts.clear();
                    self.block_read_handles.clear();
                    drop(pipeline_handles);
                    return Ok(remapped);
                }
                Err(error) => {
                    let fault_index = error.fault_operation;
                    collected.merge_from(error.results);
                    let transfer_error = batch_error_to_transfer(&error.error);

                    match transfer_error {
                        Some(SwdTransferError::WaitResponse) if self.probe.handles_wait() => {
                            return Err(SwdPortError::Transfer(SwdTransferError::WaitResponse));
                        }
                        Some(SwdTransferError::WaitResponse) => {
                            tracing::debug!("got WAIT on operation {}, retrying...", fault_index);
                            self.clear_overrun_and_sticky_err()?;
                            expanded.consume(fault_index);
                            bump_write_idle(&mut expanded, idle_cycles as u32);
                            idle_cycles = idle_cycles
                                .saturating_mul(2)
                                .min(self.settings.max_retry_idle_cycles_after_wait);
                        }
                        Some(SwdTransferError::FaultResponse) => {
                            self.clear_overrun_and_sticky_err()?;
                            return Err(SwdPortError::Transfer(SwdTransferError::FaultResponse));
                        }
                        Some(error) => return Err(SwdPortError::Transfer(error)),
                        None => return Err(SwdPortError::Probe(probe_error(&error.error))),
                    }
                }
            }
        }

        tracing::debug!(
            "Timeout in SWD transaction, aborting AP transactions after {max_retries} retries."
        );
        self.write_abort_dap().ok();
        Err(SwdPortError::Transfer(SwdTransferError::WaitResponse))
    }
}

fn decode_block_read(result: CommandResult) -> Vec<u32> {
    match result {
        CommandResult::VecU8(bytes) => {
            assert_eq!(bytes.len() % 4, 0);
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect()
        }
        _ => panic!("unexpected CommandResult variant for an AP block read"),
    }
}

fn encode_block_read(values: &[u32]) -> CommandResult {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    CommandResult::VecU8(bytes)
}

fn batch_error_to_transfer(error: &BatchError<DebugProbeError>) -> Option<SwdTransferError> {
    let debug_error = match error {
        BatchError::Specific(error) => error,
        BatchError::Probe(error) => error,
    };
    match debug_error {
        DebugProbeError::SwdTransfer(error) => Some(*error),
        _ => None,
    }
}

fn probe_error(error: &BatchError<DebugProbeError>) -> DebugProbeError {
    match error {
        BatchError::Specific(error) => DebugProbeError::Other(error.to_string()),
        BatchError::Probe(error) => DebugProbeError::Other(format!("{error:?}")),
    }
}

struct ExpansionPlan {
    expanded: SwdBatch,
    read_handles: Vec<Option<HandleId>>,
    logical: Vec<LogicalMapping>,
    pipeline_handles: Vec<Handle<()>>,
}

enum LogicalMapping {
    Passthrough {
        handle_id: HandleId,
        expanded_read: usize,
    },
    FromNext {
        handle_id: HandleId,
        expanded_read: usize,
    },
}

fn expand_batch(
    batch: &SwdBatch,
    settings: &SwdSettings,
    probe_handles_pipeline: bool,
) -> ExpansionPlan {
    let transfers: Vec<SwdOp> = batch
        .iter()
        .filter_map(|(_, op)| match op {
            SwdOp::Transfer { .. } => Some(op.clone()),
            _ => None,
        })
        .collect();

    let mut expanded = SwdBatch::new();
    let mut read_handles = Vec::new();
    let mut logical_mappings = Vec::new();
    let mut pipeline_handles = Vec::new();
    let mut transfer_index = 0usize;

    for (handle_id, op) in batch.iter() {
        let op = op.clone();
        match op {
            SwdOp::Transfer {
                port,
                addr,
                direction,
                data,
            } => {
                let need_ap_read = is_ap_read(&op);
                let buffered_write = is_ap_write(&op);
                let write_response_pending = is_write(&op) && !is_abort(&op);
                let response_in_next = need_ap_read || write_response_pending;

                let expanded_read = read_handles.len();
                schedule_transfer(&mut expanded, handle_id, port, addr, direction, data);
                if direction == Direction::Write && !probe_handles_pipeline {
                    expanded.idle(settings.num_idle_cycles_between_writes as u32);
                }
                if direction == Direction::Read {
                    if handle_id.should_capture() {
                        read_handles.push(Some(handle_id.clone()));
                    } else {
                        read_handles.push(None);
                    }
                }

                let next_transfer = transfers.get(transfer_index + 1);
                let need_extra = !probe_handles_pipeline
                    && extra_rdbuff_needed(
                        need_ap_read,
                        buffered_write,
                        write_response_pending,
                        next_transfer,
                    );

                if need_extra {
                    if write_response_pending {
                        expanded.idle(settings.idle_cycles_before_write_verify as u32);
                    }
                    let handle = expanded
                        .schedule(SwdOp::Transfer {
                            port: Port::Dp,
                            addr: DP_RDBUFF_ADDR,
                            direction: Direction::Read,
                            data: 0,
                        })
                        .map(|_| ());
                    read_handles.push(Some(handle.id().clone()));
                    pipeline_handles.push(handle);
                }

                if direction == Direction::Read && handle_id.should_capture() {
                    if response_in_next && !probe_handles_pipeline {
                        logical_mappings.push(LogicalMapping::FromNext {
                            handle_id: handle_id.clone(),
                            expanded_read,
                        });
                    } else {
                        logical_mappings.push(LogicalMapping::Passthrough {
                            handle_id: handle_id.clone(),
                            expanded_read,
                        });
                    }
                }

                transfer_index += 1;
            }
            SwdOp::Sequence(bits) => expanded.sequence(bits),
            SwdOp::Idle { cycles } => expanded.idle(cycles),
            SwdOp::Pins { out, select, wait } => {
                let _ = expanded.schedule(SwdOp::Pins { out, select, wait });
            }
        }
    }

    if !expanded.is_empty() && !probe_handles_pipeline {
        expanded.idle(settings.idle_cycles_after_transfer as u32);
    }

    ExpansionPlan {
        expanded,
        read_handles,
        logical: logical_mappings,
        pipeline_handles,
    }
}

fn extra_rdbuff_needed(
    need_ap_read: bool,
    buffered_write: bool,
    write_response_pending: bool,
    next: Option<&SwdOp>,
) -> bool {
    if let Some(next) = next {
        if is_rdbuff_read(next) {
            return false;
        }
        if need_ap_read && !is_ap_read(next) {
            return true;
        }
        if buffered_write && must_not_stall(next) {
            return true;
        }
        false
    } else {
        need_ap_read || write_response_pending
    }
}

fn schedule_transfer(
    batch: &mut SwdBatch,
    handle_id: &HandleId,
    port: Port,
    addr: u8,
    direction: Direction,
    data: u32,
) {
    let op = SwdOp::Transfer {
        port,
        addr,
        direction,
        data,
    };
    match direction {
        Direction::Read if handle_id.should_capture() => {
            batch.schedule_preserved(handle_id.clone(), op);
        }
        Direction::Read => {
            let _ = batch.schedule(op);
        }
        Direction::Write => {
            let _ = batch.schedule(op);
        }
    }
}

fn bump_write_idle(batch: &mut SwdBatch, extra: u32) {
    let remaining = batch.remaining_with_ids();
    let mut modified = Vec::with_capacity(remaining.len());
    let mut index = 0;
    while index < remaining.len() {
        let (id, op) = remaining[index].clone();
        modified.push((id, op.clone()));
        if matches!(
            op,
            SwdOp::Transfer {
                direction: Direction::Write,
                ..
            }
        ) && let Some((idle_id, SwdOp::Idle { cycles })) = remaining.get(index + 1)
        {
            modified.push((
                idle_id.clone(),
                SwdOp::Idle {
                    cycles: cycles + extra,
                },
            ));
            index += 2;
            continue;
        }
        index += 1;
    }
    batch.replace_remaining(modified);
}

fn is_ap_read(op: &SwdOp) -> bool {
    matches!(
        op,
        SwdOp::Transfer {
            port: Port::Ap,
            direction: Direction::Read,
            ..
        }
    )
}

fn is_ap_write(op: &SwdOp) -> bool {
    matches!(
        op,
        SwdOp::Transfer {
            port: Port::Ap,
            direction: Direction::Write,
            ..
        }
    )
}

fn is_write(op: &SwdOp) -> bool {
    matches!(
        op,
        SwdOp::Transfer {
            direction: Direction::Write,
            ..
        }
    )
}

fn is_abort(op: &SwdOp) -> bool {
    matches!(
        op,
        SwdOp::Transfer {
            port: Port::Dp,
            addr: DP_ABORT_ADDR,
            direction: Direction::Write,
            ..
        }
    )
}

fn is_rdbuff_read(op: &SwdOp) -> bool {
    matches!(
        op,
        SwdOp::Transfer {
            port: Port::Dp,
            addr: DP_RDBUFF_ADDR,
            direction: Direction::Read,
            ..
        }
    )
}

fn must_not_stall(op: &SwdOp) -> bool {
    match op {
        SwdOp::Transfer {
            port: Port::Dp,
            addr,
            direction,
            ..
        } => {
            is_abort(op)
                || (*direction == Direction::Read && *addr == DP_ABORT_ADDR)
                || (*direction == Direction::Read && *addr == DP_CTRL_ADDR)
        }
        _ => false,
    }
}

fn block_value_indices(read_handles: &[Option<HandleId>], count: usize) -> Vec<usize> {
    if read_handles.len() < count {
        return Vec::new();
    }
    let start = read_handles.len() - count;
    (start..read_handles.len()).collect()
}

fn remap_results(
    read_handles: Vec<Option<HandleId>>,
    logical: Vec<LogicalMapping>,
    mut expanded_results: Results,
    block_read_counts: &[usize],
) -> Results {
    let mut logical_results = Results::new();
    let mut expanded_read_values: Vec<Option<u32>> = vec![None; read_handles.len()];
    let block_aggregate_ids: HashSet<HandleId> = block_read_counts
        .iter()
        .filter_map(|&count| {
            block_value_indices(&read_handles, count)
                .last()
                .and_then(|&index| read_handles[index].clone())
        })
        .collect();

    for (index, handle_id) in read_handles.iter().enumerate() {
        if let Some(handle_id) = handle_id {
            let handle = Handle::from_parts(
                handle_id.clone(),
                Box::new(|result| match result {
                    CommandResult::U32(value) => value,
                    _ => panic!("unexpected CommandResult variant for an SWD read"),
                }),
            );
            if let Ok(value) = expanded_results.take(handle) {
                expanded_read_values[index] = Some(value);
            }
        }
    }

    for mapping in logical {
        match mapping {
            LogicalMapping::Passthrough {
                handle_id,
                expanded_read,
            } => {
                if block_aggregate_ids.contains(&handle_id) {
                    continue;
                }
                if let Some(value) = expanded_read_values[expanded_read] {
                    logical_results.push(&handle_id, CommandResult::U32(value));
                }
            }
            LogicalMapping::FromNext {
                handle_id,
                expanded_read,
            } => {
                if let Some(value) = expanded_read_values
                    .get(expanded_read + 1)
                    .and_then(|value| *value)
                {
                    logical_results.push(&handle_id, CommandResult::U32(value));
                }
            }
        }
    }

    for &count in block_read_counts {
        let indices = block_value_indices(&read_handles, count);
        let Some(&aggregate_index) = indices.last() else {
            continue;
        };
        let Some(aggregate_id) = &read_handles[aggregate_index] else {
            continue;
        };
        if !aggregate_id.should_capture() {
            continue;
        }
        let values: Vec<u32> = indices
            .iter()
            .filter_map(|index| expanded_read_values[*index])
            .collect();
        if !values.is_empty() {
            logical_results.push(aggregate_id, encode_block_read(&values));
        }
    }

    logical_results
}

impl SwdPort<'_> {
    fn clear_overrun_and_sticky_err(&mut self) -> Result<(), SwdPortError> {
        tracing::debug!("Clearing overrun and sticky error");
        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        batch.write(Port::Dp, DP_ABORT_ADDR, ABORT_CLEAR_STICKY);
        if !self.probe.handles_ap_pipeline() {
            batch.idle(
                (self.settings.idle_cycles_before_write_verify
                    + self.settings.num_idle_cycles_between_writes) as u32,
            );
            batch.idle(self.settings.idle_cycles_after_transfer as u32);
        }
        self.probe
            .run_batch(&batch)
            .map_err(|error| SwdPortError::Probe(probe_error(&error.error)))?;
        Ok(())
    }

    fn write_abort_dap(&mut self) -> Result<(), SwdPortError> {
        let mut batch = SwdBatch::new();
        batch.write(Port::Dp, DP_ABORT_ADDR, ABORT_DAP_ABORT);
        batch.idle(self.settings.num_idle_cycles_between_writes as u32);
        batch.idle(self.settings.idle_cycles_after_transfer as u32);
        self.probe
            .run_batch(&batch)
            .map_err(|error| SwdPortError::Probe(probe_error(&error.error)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::swd::mock::{MockSwdProbe, RecordedOp, ScriptedResponse};

    fn duplicate_settings(settings: &SwdSettings) -> SwdSettings {
        SwdSettings {
            num_idle_cycles_between_writes: settings.num_idle_cycles_between_writes,
            num_retries_after_wait: settings.num_retries_after_wait,
            max_retry_idle_cycles_after_wait: settings.max_retry_idle_cycles_after_wait,
            idle_cycles_before_write_verify: settings.idle_cycles_before_write_verify,
            idle_cycles_after_transfer: settings.idle_cycles_after_transfer,
        }
    }

    fn read_ops(ops: &[RecordedOp]) -> Vec<(Port, u8)> {
        ops.iter()
            .filter(|op| op.direction == Direction::Read)
            .map(|op| (op.port, op.addr))
            .collect()
    }

    fn write_count(ops: &[RecordedOp]) -> usize {
        ops.iter()
            .filter(|op| op.direction == Direction::Write)
            .count()
    }

    fn ap_write_count(ops: &[RecordedOp]) -> usize {
        ops.iter()
            .filter(|op| op.direction == Direction::Write && op.port == Port::Ap)
            .count()
    }

    fn push_clear_overrun_ok(probe: &mut MockSwdProbe) {
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(0));
    }

    fn push_ok(probe: &mut MockSwdProbe, count: usize) {
        for _ in 0..count {
            probe.push_response(ScriptedResponse::Ok(0));
        }
    }

    #[test]
    fn wait_on_third_operation_retries_from_third() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(1));
        probe.push_response(ScriptedResponse::Ok(2));
        probe.push_response(ScriptedResponse::Wait);
        push_clear_overrun_ok(&mut probe);
        probe.push_response(ScriptedResponse::Ok(3));
        push_ok(&mut probe, 3);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);

        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        port.run(batch).expect("run should succeed");

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads.len(), 5);
        assert_eq!(reads[0], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[1], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[2], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[3], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[4], (Port::Dp, DP_CTRL_ADDR));
    }

    #[test]
    fn wait_on_trailing_rdbuff_of_block_read_retries_rdbuff_only() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Wait);
        push_clear_overrun_ok(&mut probe);
        probe.push_response(ScriptedResponse::Ok(42));

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let handle = port.read_ap_block(&mut batch, 0b1000, 1);

        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(handle).unwrap(), vec![42]);

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads.len(), 4);
        assert_eq!(reads[0], (Port::Ap, 0b1000));
        assert_eq!(reads[1], (Port::Dp, DP_RDBUFF_ADDR));
        assert_eq!(reads[2], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[3], (Port::Dp, DP_RDBUFF_ADDR));
    }

    #[test]
    fn wait_on_trailing_rdbuff_of_posted_write_retries_rdbuff_only() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Wait);
        push_clear_overrun_ok(&mut probe);
        push_ok(&mut probe, 4);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        port.write_ap_block(&mut batch, 0b1000, &[0x1234_5678]);
        port.run(batch).expect("run should succeed");

        let ops = probe.transfer_ops();
        assert_eq!(ap_write_count(&ops), 1);
        let reads = read_ops(&ops);
        assert_eq!(reads.len(), 3);
        assert_eq!(reads[0], (Port::Dp, DP_RDBUFF_ADDR));
        assert_eq!(reads[1], (Port::Dp, DP_CTRL_ADDR));
        assert_eq!(reads[2], (Port::Dp, DP_RDBUFF_ADDR));
    }

    #[test]
    fn retry_doubles_idle_cycles_until_cap() {
        let settings = SwdSettings {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 4,
            max_retry_idle_cycles_after_wait: 8,
            idle_cycles_before_write_verify: 0,
            idle_cycles_after_transfer: 0,
        };
        let mut probe = MockSwdProbe::with_settings(duplicate_settings(&settings));
        for _ in 0..3 {
            probe.push_response(ScriptedResponse::Ok(0));
            probe.push_response(ScriptedResponse::Wait);
            push_clear_overrun_ok(&mut probe);
        }
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(0));

        let mut batch = SwdBatch::new();
        batch.write(Port::Ap, 0b1000, 0x1);
        let mut port = SwdPort::new(&mut probe, duplicate_settings(&settings));
        port.run(batch).expect("run should succeed");

        assert_eq!(ap_write_count(&probe.transfer_ops()), 1);
    }

    #[test]
    fn retry_gives_up_after_num_retries_after_wait() {
        let settings = SwdSettings {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 2,
            max_retry_idle_cycles_after_wait: 128,
            idle_cycles_before_write_verify: 0,
            idle_cycles_after_transfer: 0,
        };
        let mut probe = MockSwdProbe::with_settings(duplicate_settings(&settings));
        for _ in 0..2 {
            probe.push_response(ScriptedResponse::Wait);
            push_clear_overrun_ok(&mut probe);
        }
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(0));

        let mut batch = SwdBatch::new();
        batch.write(Port::Ap, 0b1000, 0x1);
        let mut port = SwdPort::new(&mut probe, duplicate_settings(&settings));
        let error = expect_err(port.run(batch), "run should fail");
        assert_eq!(
            error,
            SwdPortError::Transfer(SwdTransferError::WaitResponse)
        );

        let abort_writes = probe
            .transfer_ops()
            .iter()
            .filter(|op| {
                op.direction == Direction::Write
                    && op.port == Port::Dp
                    && op.addr == DP_ABORT_ADDR
                    && op.data == ABORT_DAP_ABORT
            })
            .count();
        assert_eq!(abort_writes, 1);
    }

    #[test]
    fn fault_clears_sticky_error_and_returns_fault() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Fault);
        push_ok(&mut probe, 3);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let error = expect_err(port.run(batch), "run should fail");
        assert_eq!(
            error,
            SwdPortError::Transfer(SwdTransferError::FaultResponse)
        );

        let sticky_clears = probe
            .transfer_ops()
            .iter()
            .filter(|op| {
                op.direction == Direction::Write
                    && op.port == Port::Dp
                    && op.addr == DP_ABORT_ADDR
                    && op.data == ABORT_CLEAR_STICKY
            })
            .count();
        assert_eq!(sticky_clears, 1);
    }

    #[test]
    fn handles_wait_probe_gets_no_wait_retry_but_fault_still_clears_sticky() {
        let settings = SwdSettings {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 100,
            max_retry_idle_cycles_after_wait: 128,
            idle_cycles_before_write_verify: 0,
            idle_cycles_after_transfer: 0,
        };
        {
            let mut probe =
                MockSwdProbe::with_settings(duplicate_settings(&settings)).handles_wait();
            probe.push_response(ScriptedResponse::Wait);

            let mut batch = SwdBatch::new();
            let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
            let mut port = SwdPort::new(&mut probe, duplicate_settings(&settings));
            let error = expect_err(port.run(batch), "run should fail");
            assert_eq!(
                error,
                SwdPortError::Transfer(SwdTransferError::WaitResponse)
            );
            assert_eq!(probe.transfer_ops().len(), 1);
        }

        let mut probe = MockSwdProbe::with_settings(duplicate_settings(&settings));
        probe.push_response(ScriptedResponse::Fault);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        let mut port = SwdPort::new(&mut probe, duplicate_settings(&settings));
        let error = expect_err(port.run(batch), "run should fail");
        assert_eq!(
            error,
            SwdPortError::Transfer(SwdTransferError::FaultResponse)
        );
        let sticky_clears = probe
            .transfer_ops()
            .iter()
            .filter(|op| op.data == ABORT_CLEAR_STICKY)
            .count();
        assert_eq!(sticky_clears, 1);
    }

    #[test]
    fn block_read_of_16_values_sends_17_reads_and_returns_16_values() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        for value in 1..=16 {
            probe.push_response(ScriptedResponse::Ok(value));
        }
        push_ok(&mut probe, 1);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let handle = port.read_ap_block(&mut batch, 0b1000, 16);
        let mut results = port.run(batch).expect("run should succeed");
        let values = results.take(handle).unwrap();
        assert_eq!(values.len(), 16);
        assert_eq!(values, (1..=16).collect::<Vec<_>>());

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads.len(), 17);
    }

    #[test]
    fn posting_probe_reads_ap_register_without_rdbuff() {
        let mut probe = MockSwdProbe::new().handles_ap_pipeline();
        probe.set_read_value(Port::Ap, 0b0100, 42);

        let mut batch = SwdBatch::new();
        let handle = batch.read(Port::Ap, 0b0100);

        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(handle).unwrap(), 42);

        assert_eq!(read_ops(&probe.transfer_ops()), vec![(Port::Ap, 0b0100)]);
        assert!(probe.idles().is_empty());
    }

    #[test]
    fn posting_probe_block_read_keeps_every_result() {
        let mut probe = MockSwdProbe::new().handles_ap_pipeline();
        probe.set_read_sequence(Port::Ap, 0b1000, vec![11, 22, 33]);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let handle = port.read_ap_block(&mut batch, 0b1000, 3);
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(handle).unwrap(), vec![11, 22, 33]);

        assert_eq!(read_ops(&probe.transfer_ops()).len(), 3);
    }

    #[test]
    fn posting_probe_block_write_adds_no_verify_read() {
        let mut probe = MockSwdProbe::new().handles_ap_pipeline();

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        port.write_ap_block(&mut batch, 0b1000, &[1, 2, 3]);
        port.run(batch).expect("run should succeed");

        let ops = probe.transfer_ops();
        assert_eq!(ap_write_count(&ops), 3);
        assert!(read_ops(&ops).is_empty());
        assert!(probe.idles().is_empty());
    }

    #[test]
    fn block_read_of_one_drops_first_result() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(99));
        probe.push_response(ScriptedResponse::Ok(42));
        push_ok(&mut probe, 1);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let handle = port.read_ap_block(&mut batch, 0b1000, 1);
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(handle).unwrap(), vec![42]);

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads, vec![(Port::Ap, 0b1000), (Port::Dp, DP_RDBUFF_ADDR)]);
        assert!(!probe.capture_flags()[0]);
        assert!(probe.capture_flags()[1]);
    }

    #[test]
    fn block_read_of_two_drops_first_result() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(11));
        probe.push_response(ScriptedResponse::Ok(22));
        push_ok(&mut probe, 1);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let handle = port.read_ap_block(&mut batch, 0b1000, 2);
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(handle).unwrap(), vec![11, 22]);

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(
            reads,
            vec![
                (Port::Ap, 0b1000),
                (Port::Ap, 0b1000),
                (Port::Dp, DP_RDBUFF_ADDR),
            ]
        );
        assert!(!probe.capture_flags()[0]);
    }

    #[test]
    fn block_write_of_one_includes_trailing_rdbuff() {
        let mut probe = MockSwdProbe::new();
        push_ok(&mut probe, 3);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        port.write_ap_block(&mut batch, 0b1000, &[0xA]);
        port.run(batch).expect("run should succeed");

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads.last(), Some(&(Port::Dp, DP_RDBUFF_ADDR)));
        assert_eq!(write_count(&probe.transfer_ops()), 1);
    }

    #[test]
    fn block_write_of_16_includes_trailing_rdbuff() {
        let mut probe = MockSwdProbe::new();
        push_ok(&mut probe, 18);

        let mut batch = SwdBatch::new();
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        port.write_ap_block(&mut batch, 0b1000, &[0x1; 16]);
        port.run(batch).expect("run should succeed");

        assert_eq!(write_count(&probe.transfer_ops()), 16);
        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(reads.last(), Some(&(Port::Dp, DP_RDBUFF_ADDR)));
    }

    #[test]
    fn ap_read_followed_by_dp_read_includes_rdbuff_between() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(10));
        probe.push_response(ScriptedResponse::Ok(20));
        push_ok(&mut probe, 1);

        let mut batch = SwdBatch::new();
        let ap = batch.read(Port::Ap, 0b1000);
        let dp = batch.read(Port::Dp, DP_CTRL_ADDR);
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(ap).unwrap(), 10);
        assert_eq!(results.take(dp).unwrap(), 20);

        let reads = read_ops(&probe.transfer_ops());
        assert_eq!(
            reads,
            vec![
                (Port::Ap, 0b1000),
                (Port::Dp, DP_RDBUFF_ADDR),
                (Port::Dp, DP_CTRL_ADDR),
            ]
        );
    }

    /// TAR is at 0x04, so bits 2 and 3 are 0b0100.
    const AP_TAR_ADDR: u8 = 0b0100;
    /// DRW is at 0x0C, so bits 2 and 3 are 0b1100.
    const AP_DRW_ADDR: u8 = 0b1100;

    fn scattered_read_batch(addresses: &[u32]) -> (SwdBatch, Vec<Handle<u32>>) {
        let mut batch = SwdBatch::new();
        let mut handles = Vec::new();
        for &address in addresses {
            batch.write(Port::Ap, AP_TAR_ADDR, address);
            handles.push(batch.read(Port::Ap, AP_DRW_ADDR));
        }
        (batch, handles)
    }

    #[test]
    fn scattered_read_takes_each_value_from_the_rdbuff_that_follows_it() {
        let mut probe = MockSwdProbe::new();
        // The TAR write, the DRW read that posts the access, then the RDBUFF that carries it.
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(11));
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(22));

        let (batch, handles) = scattered_read_batch(&[0x2000_0000, 0x2000_0040]);
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let mut results = port.run(batch).expect("run should succeed");

        let values: Vec<u32> = handles
            .into_iter()
            .map(|handle| results.take(handle).unwrap())
            .collect();
        assert_eq!(values, vec![11, 22]);

        let ops = probe.transfer_ops();
        assert_eq!(ap_write_count(&ops), 2);
        assert_eq!(
            read_ops(&ops),
            vec![
                (Port::Ap, AP_DRW_ADDR),
                (Port::Dp, DP_RDBUFF_ADDR),
                (Port::Ap, AP_DRW_ADDR),
                (Port::Dp, DP_RDBUFF_ADDR),
            ]
        );
    }

    #[test]
    fn scattered_read_on_a_posting_probe_adds_nothing() {
        let mut probe = MockSwdProbe::new().handles_ap_pipeline();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(11));
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(22));

        let (batch, handles) = scattered_read_batch(&[0x2000_0000, 0x2000_0040]);
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let mut results = port.run(batch).expect("run should succeed");

        let values: Vec<u32> = handles
            .into_iter()
            .map(|handle| results.take(handle).unwrap())
            .collect();
        assert_eq!(values, vec![11, 22]);

        let ops = probe.transfer_ops();
        assert_eq!(ap_write_count(&ops), 2);
        assert_eq!(
            read_ops(&ops),
            vec![(Port::Ap, AP_DRW_ADDR), (Port::Ap, AP_DRW_ADDR)]
        );
        assert!(probe.idles().is_empty());
    }

    #[test]
    fn dropped_handle_sets_should_capture_false() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(1));
        probe.push_response(ScriptedResponse::Ok(2));
        push_clear_overrun_ok(&mut probe);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, DP_CTRL_ADDR);
        let kept = batch.read(Port::Dp, DP_CTRL_ADDR);
        let mut port = SwdPort::new(&mut probe, SwdSettings::default());
        let mut results = port.run(batch).expect("run should succeed");
        assert_eq!(results.take(kept).unwrap(), 2);

        assert!(!probe.capture_flags()[0]);
        assert!(probe.capture_flags()[1]);
    }

    fn run_read(probe: &mut MockSwdProbe, port: Port, addr: u8) -> (u32, Vec<RecordedOp>) {
        let mut batch = SwdBatch::new();
        let handle = batch.read(port, addr);
        let mut swd_port = SwdPort::new(probe, SwdSettings::default());
        let mut results = swd_port.run(batch).expect("run should succeed");
        (results.take(handle).unwrap(), probe.transfer_ops())
    }

    fn run_writes(probe: &mut MockSwdProbe, batch: SwdBatch) -> Vec<RecordedOp> {
        let mut swd_port = SwdPort::new(probe, SwdSettings::default());
        swd_port.run(batch).expect("run should succeed");
        probe.transfer_ops()
    }

    #[test]
    fn read_register() {
        let read_value = 12;
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Ok(read_value));
        push_ok(&mut probe, 1);

        let (value, reads) = run_read(&mut probe, Port::Ap, 0b0100);
        assert_eq!(value, read_value);
        assert_eq!(
            read_ops(&reads),
            vec![(Port::Ap, 0b0100), (Port::Dp, DP_RDBUFF_ADDR)]
        );
    }

    #[test]
    fn read_register_with_wait_response() {
        let read_value = 47;
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Wait);
        push_clear_overrun_ok(&mut probe);
        probe.push_response(ScriptedResponse::Ok(read_value));
        push_ok(&mut probe, 1);

        let (value, _) = run_read(&mut probe, Port::Ap, 0b0100);
        assert_eq!(value, read_value);
    }

    #[test]
    fn write_register() {
        let mut probe = MockSwdProbe::new();
        push_ok(&mut probe, 2);

        let mut batch = SwdBatch::new();
        batch.write(Port::Ap, 0b0100, 0x123);
        let ops = run_writes(&mut probe, batch);

        assert_eq!(ap_write_count(&ops), 1);
        assert_eq!(read_ops(&ops), vec![(Port::Dp, DP_RDBUFF_ADDR)]);
    }

    #[test]
    fn write_register_with_wait_response() {
        let mut probe = MockSwdProbe::new();
        probe.push_response(ScriptedResponse::Ok(0));
        probe.push_response(ScriptedResponse::Wait);
        push_clear_overrun_ok(&mut probe);
        push_ok(&mut probe, 2);

        let mut batch = SwdBatch::new();
        batch.write(Port::Ap, 0b0100, 0x123);
        run_writes(&mut probe, batch);
    }

    mod transfer_handling {
        use super::*;

        #[test]
        fn single_dp_register_read() {
            let register_value = 32354;
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(register_value));
            push_ok(&mut probe, 1);

            let (value, reads) = run_read(&mut probe, Port::Dp, 0);
            assert_eq!(value, register_value);
            assert_eq!(read_ops(&reads), vec![(Port::Dp, 0)]);
        }

        #[test]
        fn single_ap_register_read() {
            let register_value = 0x11_22_33_44u32;
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(0));
            probe.push_response(ScriptedResponse::Ok(register_value));
            push_ok(&mut probe, 1);

            let (value, reads) = run_read(&mut probe, Port::Ap, 0);
            assert_eq!(value, register_value);
            assert_eq!(
                read_ops(&reads),
                vec![(Port::Ap, 0), (Port::Dp, DP_RDBUFF_ADDR)]
            );
        }

        #[test]
        fn ap_then_dp_register_read() {
            let ap_read_value = 0x123223;
            let dp_read_value = 0xFFAABB;
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(0));
            probe.push_response(ScriptedResponse::Ok(ap_read_value));
            probe.push_response(ScriptedResponse::Ok(dp_read_value));
            push_ok(&mut probe, 1);

            let mut batch = SwdBatch::new();
            let ap = batch.read(Port::Ap, 0b0100);
            let dp = batch.read(Port::Dp, 0);
            let mut port = SwdPort::new(&mut probe, SwdSettings::default());
            let mut results = port.run(batch).expect("run should succeed");
            assert_eq!(results.take(ap).unwrap(), ap_read_value);
            assert_eq!(results.take(dp).unwrap(), dp_read_value);

            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![
                    (Port::Ap, 0b0100),
                    (Port::Dp, DP_RDBUFF_ADDR),
                    (Port::Dp, 0),
                ]
            );
        }

        #[test]
        fn dp_then_ap_register_read() {
            let ap_read_value = 0x123223;
            let dp_read_value = 0xFFAABB;
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(dp_read_value));
            probe.push_response(ScriptedResponse::Ok(0));
            probe.push_response(ScriptedResponse::Ok(ap_read_value));
            push_ok(&mut probe, 1);

            let mut batch = SwdBatch::new();
            let dp = batch.read(Port::Dp, 0);
            let ap = batch.read(Port::Ap, 0b0100);
            let mut port = SwdPort::new(&mut probe, SwdSettings::default());
            let mut results = port.run(batch).expect("run should succeed");
            assert_eq!(results.take(dp).unwrap(), dp_read_value);
            assert_eq!(results.take(ap).unwrap(), ap_read_value);

            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![
                    (Port::Dp, 0),
                    (Port::Ap, 0b0100),
                    (Port::Dp, DP_RDBUFF_ADDR),
                ]
            );
        }

        #[test]
        fn multiple_ap_read() {
            let ap_read_values = [1, 2];
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(0));
            probe.push_response(ScriptedResponse::Ok(ap_read_values[0]));
            probe.push_response(ScriptedResponse::Ok(ap_read_values[1]));
            push_ok(&mut probe, 1);

            let mut batch = SwdBatch::new();
            let first = batch.read(Port::Ap, 0b0100);
            let second = batch.read(Port::Ap, 0b0100);
            let mut port = SwdPort::new(&mut probe, SwdSettings::default());
            let mut results = port.run(batch).expect("run should succeed");
            assert_eq!(results.take(first).unwrap(), ap_read_values[0]);
            assert_eq!(results.take(second).unwrap(), ap_read_values[1]);

            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![
                    (Port::Ap, 0b0100),
                    (Port::Ap, 0b0100),
                    (Port::Dp, DP_RDBUFF_ADDR),
                ]
            );
        }

        #[test]
        fn multiple_dp_read() {
            let dp_read_values = [1, 2];
            let mut probe = MockSwdProbe::new();
            probe.push_response(ScriptedResponse::Ok(dp_read_values[0]));
            probe.push_response(ScriptedResponse::Ok(dp_read_values[1]));
            push_ok(&mut probe, 1);

            let mut batch = SwdBatch::new();
            let first = batch.read(Port::Dp, DP_CTRL_ADDR);
            let second = batch.read(Port::Dp, DP_CTRL_ADDR);
            let mut port = SwdPort::new(&mut probe, SwdSettings::default());
            let mut results = port.run(batch).expect("run should succeed");
            assert_eq!(results.take(first).unwrap(), dp_read_values[0]);
            assert_eq!(results.take(second).unwrap(), dp_read_values[1]);

            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![(Port::Dp, DP_CTRL_ADDR), (Port::Dp, DP_CTRL_ADDR)]
            );
        }

        #[test]
        fn single_dp_register_write() {
            let mut probe = MockSwdProbe::new();
            push_ok(&mut probe, 1);

            let mut batch = SwdBatch::new();
            batch.write(Port::Dp, DP_ABORT_ADDR, 0x1234_5678);
            run_writes(&mut probe, batch);

            assert_eq!(write_count(&probe.transfer_ops()), 1);
            assert!(read_ops(&probe.transfer_ops()).is_empty());
        }

        #[test]
        fn single_ap_register_write() {
            let mut probe = MockSwdProbe::new();
            push_ok(&mut probe, 2);

            let mut batch = SwdBatch::new();
            batch.write(Port::Ap, 0, 0x1234_5678);
            run_writes(&mut probe, batch);

            assert_eq!(ap_write_count(&probe.transfer_ops()), 1);
            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![(Port::Dp, DP_RDBUFF_ADDR)]
            );
        }

        #[test]
        fn multiple_ap_register_write() {
            let mut probe = MockSwdProbe::new();
            push_ok(&mut probe, 3);

            let mut batch = SwdBatch::new();
            batch.write(Port::Ap, 0, 0x1234_5678);
            batch.write(Port::Ap, 0, 0xABABABAB);
            run_writes(&mut probe, batch);

            assert_eq!(ap_write_count(&probe.transfer_ops()), 2);
            assert_eq!(
                read_ops(&probe.transfer_ops()),
                vec![(Port::Dp, DP_RDBUFF_ADDR)]
            );
        }
    }

    fn expect_err<T, E: std::fmt::Debug>(result: Result<T, E>, message: &str) -> E {
        match result {
            Ok(_) => panic!("{message}: got Ok"),
            Err(error) => error,
        }
    }
}
