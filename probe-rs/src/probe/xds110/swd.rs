//! SWD batches, run with `OCD_DAP_REQUEST`.

use super::{
    MAX_DATA_BLOCK, OCD_DAP_REQUEST, SC_ERR_SWD_FAULT, SC_ERR_SWD_PARITY, SC_ERR_SWD_PROTOCOL,
    SC_ERR_SWD_WAIT, XDS_SET_SRST, XDS_SET_TRST, Xds110, Xds110Error, jtag::XdsState,
};
use crate::probe::{
    BatchExecutionError, CommandResult, DebugProbeError, HandleId, Results, TapState, WireProtocol,
    swd::{Direction, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError},
};

/// Most reads in one request: the response holds 4 KB of values.
const MAX_READS: usize = MAX_DATA_BLOCK / 4;

/// The ORUNDETECT bit of CTRL/STAT. The firmware does not handle overrun detection.
const ORUNDETECT: u32 = 1;

/// The request byte of an SWD transfer, with start, parity and park bits.
fn request_byte(port: Port, addr: u8, direction: Direction) -> u8 {
    let ap = (port == Port::Ap) as u8;
    let read = (direction == Direction::Read) as u8;
    let a2 = (addr >> 2) & 1;
    let a3 = (addr >> 3) & 1;
    let parity = (ap + read + a2 + a3) & 1;
    1 | ap << 1 | read << 2 | a2 << 3 | a3 << 4 | parity << 5 | 1 << 7
}

fn transfer_error(error: Xds110Error) -> DebugProbeError {
    match error {
        Xds110Error::Command { code, .. } => match code {
            SC_ERR_SWD_WAIT => SwdTransferError::WaitResponse.into(),
            SC_ERR_SWD_FAULT => SwdTransferError::FaultResponse.into(),
            SC_ERR_SWD_PARITY => SwdTransferError::IncorrectParity.into(),
            SC_ERR_SWD_PROTOCOL => SwdTransferError::Protocol.into(),
            _ => error.into(),
        },
        error => error.into(),
    }
}

/// Transfers queued for one `OCD_DAP_REQUEST`.
#[derive(Default)]
struct DapRequest {
    requests: Vec<u8>,
    reads: Vec<HandleId>,
    /// Index of the first queued operation in the batch.
    first_operation: usize,
}

impl Xds110 {
    fn run_dap_request(
        &mut self,
        request: &mut DapRequest,
        results: &mut Results,
    ) -> Result<(), BatchExecutionError<DebugProbeError>> {
        if request.requests.is_empty() {
            return Ok(());
        }
        let mut payload = Vec::with_capacity(request.requests.len() + 2);
        payload.push(OCD_DAP_REQUEST);
        payload.append(&mut request.requests);
        payload.push(0);

        match self.usb.execute(&payload, request.reads.len() * 4) {
            Ok(values) => {
                for (id, value) in request.reads.drain(..).zip(values.as_chunks::<4>().0) {
                    if id.should_capture() {
                        results.push(&id, CommandResult::U32(u32::from_le_bytes(*value)));
                    }
                }
                Ok(())
            }
            Err(error) => {
                request.reads.clear();
                // The firmware reports one error for the request, not which transfer failed.
                Err(BatchExecutionError::new_from_debug_probe_at(
                    transfer_error(error),
                    std::mem::take(results),
                    request.first_operation,
                ))
            }
        }
    }

    /// Sends a sequence on SWDIO/TMS, which leaves the probe in JTAG mode.
    fn run_sequence(&mut self, bits: impl Iterator<Item = bool>) -> Result<(), DebugProbeError> {
        let state = self.tms_sequence(bits)?;
        if self.protocol == WireProtocol::Jtag {
            // The bits can pass Test-Logic-Reset or Update-IR and still end in the same state.
            self.jtag_state.loaded_ir = None;
            self.jtag_state.tap_state = match state.stable() {
                Some(state) => state,
                None => {
                    self.goto_state(XdsState::Reset)?;
                    TapState::TestLogicReset
                }
            };
        }
        Ok(())
    }
}

impl SwdProbe for Xds110 {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();
        let mut request = DapRequest::default();

        for (index, (id, op)) in batch.iter().enumerate() {
            let failed = |error: DebugProbeError, results: &mut Results| {
                BatchExecutionError::new_from_debug_probe_at(error, std::mem::take(results), index)
            };
            match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
                    if self.protocol != WireProtocol::Swd {
                        return Err(failed(
                            DebugProbeError::CommandNotSupportedByProbe {
                                command_name: "SWD transfer in JTAG mode",
                            },
                            &mut results,
                        ));
                    }
                    if !self.swd_connected {
                        self.swd_connect().map_err(|e| failed(e, &mut results))?;
                    }

                    let size = if *direction == Direction::Read { 1 } else { 5 };
                    if request.requests.len() + size + 1 > MAX_DATA_BLOCK
                        || (*direction == Direction::Read && request.reads.len() == MAX_READS)
                    {
                        self.run_dap_request(&mut request, &mut results)?;
                    }
                    if request.requests.is_empty() {
                        request.first_operation = index;
                    }

                    request
                        .requests
                        .push(request_byte(*port, *addr, *direction));
                    match direction {
                        Direction::Read => request.reads.push(id.clone()),
                        Direction::Write => {
                            let mut data = *data;
                            if *port == Port::Dp && *addr == 4 {
                                data &= !ORUNDETECT;
                            }
                            request.requests.extend_from_slice(&data.to_le_bytes());
                        }
                    }
                }
                SwdOp::Sequence(bits) => {
                    self.run_dap_request(&mut request, &mut results)?;
                    tracing::debug!("XDS110: sequence of {} bits on TMS", bits.len());
                    self.run_sequence(bits.iter())
                        .map_err(|e| failed(e, &mut results))?;
                }
                // The firmware times the transfers itself, and clocking TCK would switch the
                // probe to JTAG.
                SwdOp::Idle { .. } => {}
                SwdOp::Pins { out, select, .. } => {
                    self.run_dap_request(&mut request, &mut results)?;
                    let pins = || -> Result<(), DebugProbeError> {
                        if select.swclk_tck() || select.swdio_tms() || select.tdi() || select.tdo()
                        {
                            return Err(DebugProbeError::CommandNotSupportedByProbe {
                                command_name: "swj_pins",
                            });
                        }
                        Ok(())
                    };
                    pins().map_err(|e| failed(e, &mut results))?;
                    self.reset_line
                        .check_pins(*out, *select)
                        .map_err(|e| failed(e, &mut results))?;
                    if select.nreset() {
                        self.command(&[XDS_SET_SRST, out.nreset() as u8])
                            .map_err(|e| failed(e, &mut results))?;
                    }
                    if select.ntrst() {
                        self.command(&[XDS_SET_TRST, out.ntrst() as u8])
                            .map_err(|e| failed(e, &mut results))?;
                    }
                }
            }
        }

        self.run_dap_request(&mut request, &mut results)?;
        Ok(results)
    }

    fn handles_wait(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_bytes_match_openocd() {
        // OpenOCD's swd_cmd(): DP read of DPIDR is 0xa5, AP read of DRW 0x9f, DP write of
        // SELECT 0xb1.
        assert_eq!(request_byte(Port::Dp, 0x0, Direction::Read), 0xa5);
        assert_eq!(request_byte(Port::Ap, 0xc, Direction::Read), 0x9f);
        assert_eq!(request_byte(Port::Dp, 0x8, Direction::Write), 0xb1);
    }
}
