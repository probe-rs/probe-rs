//! Mock [`SwdProbe`] for host tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::probe::SwdSettings;

use super::{Direction, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError};
use crate::probe::{
    BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbe, DebugProbeError,
    Results, WireProtocol,
};

/// Scripted response for one SWD transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptedResponse {
    /// A successful transfer with `value`.
    Ok(u32),
    /// A WAIT acknowledgement.
    Wait,
    /// A FAULT acknowledgement.
    Fault,
}

/// One recorded SWD transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedOp {
    /// The debug port or access port.
    pub port: Port,
    /// Bits 2 and 3 of the register address.
    pub addr: u8,
    /// The transfer direction.
    pub direction: Direction,
    /// The write data. A read passes zero.
    pub data: u32,
}

/// One recorded SWD sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedSequence {
    /// The output bit sequence.
    pub bits: BitSequence,
}

/// One recorded pins operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedPins {
    /// The output levels.
    pub out: u8,
    /// The pin select mask.
    pub select: u8,
    /// The maximum wait time.
    pub wait: std::time::Duration,
}

/// Mock probe that records SWD operations.
#[derive(Debug)]
pub(crate) struct MockSwdProbe {
    operations: Arc<Mutex<Vec<RecordedOp>>>,
    sequences: Arc<Mutex<Vec<RecordedSequence>>>,
    responses: Vec<ScriptedResponse>,
    response_index: usize,
    read_values: HashMap<(Port, u8), u32>,
    read_sequences: HashMap<(Port, u8), Vec<u32>>,
    handles_wait: bool,
    handles_ap_pipeline: bool,
    capture_flags: Arc<Mutex<Vec<bool>>>,
    idles: Arc<Mutex<Vec<u32>>>,
    pins: Arc<Mutex<Vec<RecordedPins>>>,
}

impl MockSwdProbe {
    /// Create an empty mock probe.
    pub(crate) fn new() -> Self {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
            sequences: Arc::new(Mutex::new(Vec::new())),
            responses: Vec::new(),
            response_index: 0,
            read_values: HashMap::new(),
            read_sequences: HashMap::new(),
            handles_wait: false,
            handles_ap_pipeline: false,
            capture_flags: Arc::new(Mutex::new(Vec::new())),
            idles: Arc::new(Mutex::new(Vec::new())),
            pins: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Return a handle to the recorded operations.
    pub(crate) fn shared_operations(&self) -> Arc<Mutex<Vec<RecordedOp>>> {
        self.operations.clone()
    }

    /// Return a handle to the recorded pins operations.
    pub(crate) fn shared_pins(&self) -> Arc<Mutex<Vec<RecordedPins>>> {
        self.pins.clone()
    }

    /// Return a handle to the recorded sequences.
    pub(crate) fn shared_sequences(&self) -> Arc<Mutex<Vec<RecordedSequence>>> {
        self.sequences.clone()
    }

    /// Return true when the mock should not retry WAIT.
    pub(crate) fn handles_wait(mut self) -> Self {
        self.handles_wait = true;
        self
    }

    /// Return one value for every read, as a probe that posts AP reads does.
    pub(crate) fn handles_ap_pipeline(mut self) -> Self {
        self.handles_ap_pipeline = true;
        self
    }

    /// Create a mock probe. `settings` is unused.
    pub(crate) fn with_settings(_settings: SwdSettings) -> Self {
        Self::new()
    }

    /// Queue the next transfer response.
    pub(crate) fn push_response(&mut self, response: ScriptedResponse) {
        self.responses.push(response);
    }

    /// Return a fixed value for reads of one register.
    pub(crate) fn set_read_value(&mut self, port: Port, addr: u8, value: u32) {
        self.read_values.insert((port, addr), value);
    }

    /// Return a sequence of values for repeated reads of one register.
    pub(crate) fn set_read_sequence(&mut self, port: Port, addr: u8, values: Vec<u32>) {
        self.read_sequences.insert((port, addr), values);
    }

    fn scripted_read(&mut self, port: Port, addr: u8) -> ScriptedResponse {
        if let Some(values) = self.read_sequences.get_mut(&(port, addr))
            && let Some(value) = values.first().copied()
        {
            if values.len() > 1 {
                values.remove(0);
            }
            return ScriptedResponse::Ok(value);
        }
        self.next_queued_response(port, addr)
    }

    fn next_queued_response(&mut self, port: Port, addr: u8) -> ScriptedResponse {
        if let Some(response) = self.responses.get(self.response_index) {
            self.response_index += 1;
            return *response;
        }
        if let Some(value) = self.read_values.get(&(port, addr)) {
            return ScriptedResponse::Ok(*value);
        }
        ScriptedResponse::Ok(0)
    }

    fn next_response(&mut self, port: Port, addr: u8, direction: Direction) -> ScriptedResponse {
        if direction == Direction::Read {
            self.scripted_read(port, addr)
        } else if let Some(response) = self.responses.get(self.response_index) {
            self.response_index += 1;
            *response
        } else {
            ScriptedResponse::Ok(0)
        }
    }

    /// Return the recorded transfer operations.
    pub(crate) fn transfer_ops(&self) -> Vec<RecordedOp> {
        self.operations.lock().unwrap().clone()
    }

    /// Return whether each transfer had capture enabled.
    pub(crate) fn capture_flags(&self) -> Vec<bool> {
        self.capture_flags.lock().unwrap().clone()
    }

    /// Return the idle cycle counts of the executed batches.
    pub(crate) fn idles(&self) -> Vec<u32> {
        self.idles.lock().unwrap().clone()
    }
}

impl DebugProbe for MockSwdProbe {
    fn get_name(&self) -> &str {
        "mock swd"
    }

    fn speed_khz(&self) -> u32 {
        0
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn select_protocol(&mut self, _protocol: WireProtocol) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

impl SwdProbe for MockSwdProbe {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();

        for (fault_operation, (id, op)) in batch.iter().enumerate() {
            match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
                    let should_capture = id.should_capture();
                    self.operations.lock().unwrap().push(RecordedOp {
                        port: *port,
                        addr: *addr,
                        direction: *direction,
                        data: *data,
                    });
                    self.capture_flags.lock().unwrap().push(should_capture);

                    let response = self.next_response(*port, *addr, *direction);

                    match response {
                        ScriptedResponse::Ok(value) => {
                            if *direction == Direction::Read && should_capture {
                                results.push(id, CommandResult::U32(value));
                            }
                        }
                        ScriptedResponse::Wait => {
                            return Err(BatchExecutionError {
                                error: BatchError::Specific(DebugProbeError::SwdTransfer(
                                    SwdTransferError::WaitResponse,
                                )),
                                results,
                                fault_operation,
                            });
                        }
                        ScriptedResponse::Fault => {
                            return Err(BatchExecutionError {
                                error: BatchError::Specific(DebugProbeError::SwdTransfer(
                                    SwdTransferError::FaultResponse,
                                )),
                                results,
                                fault_operation,
                            });
                        }
                    }
                }
                SwdOp::Idle { cycles } => self.idles.lock().unwrap().push(*cycles),
                SwdOp::Sequence(bits) => {
                    self.sequences
                        .lock()
                        .unwrap()
                        .push(RecordedSequence { bits: bits.clone() });
                }
                SwdOp::Pins { out, select, wait } => {
                    self.pins.lock().unwrap().push(RecordedPins {
                        out: out.0,
                        select: select.0,
                        wait: *wait,
                    });
                }
            }
        }

        Ok(results)
    }

    fn handles_wait(&self) -> bool {
        self.handles_wait
    }

    fn handles_ap_pipeline(&self) -> bool {
        self.handles_ap_pipeline
    }
}
