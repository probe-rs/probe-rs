//! Crate-public structures and utilities to be shared between probes.

use std::iter;

use anyhow::anyhow;
use bitfield::bitfield;
use bitvec::prelude::*;
use probe_rs_target::ScanChainElement;

use crate::probe::{
    BatchExecutionError, ChainParams, DebugProbe, DebugProbeError, DeferredResultSet, JTAGAccess,
    JtagChainItem, JtagCommandQueue,
};

pub(crate) fn bits_to_byte(bits: impl IntoIterator<Item = bool>) -> u32 {
    let mut bit_val = 0u32;

    for (index, bit) in bits.into_iter().take(32).enumerate() {
        if bit {
            bit_val |= 1 << index;
        }
    }

    bit_val
}

bitfield! {
    /// A JTAG IDCODE.
    /// Identifies a particular Test Access Port (TAP) on the JTAG scan chain.
    #[derive(Copy, Clone, Eq, PartialEq)]
    pub struct IdCode(u32);
    impl Debug;

    u8;
    /// The IDCODE version.
    pub version, set_version: 31, 28;

    u16;
    /// The part number.
    pub part_number, set_part_number: 27, 12;

    /// The JEDEC JEP-106 Manufacturer ID.
    pub manufacturer, set_manufacturer: 11, 1;

    u8;
    /// The continuation code of the JEDEC JEP-106 Manufacturer ID.
    pub manufacturer_continuation, set_manufacturer_continuation: 11, 8;

    /// The identity code of the JEDEC JEP-106 Manufacturer ID.
    pub manufacturer_identity, set_manufacturer_identity: 7, 1;

    bool;
    /// The least-significant bit.
    /// Always set.
    pub lsbit, set_lsbit: 0;
}

impl std::fmt::Display for IdCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(mfn) = self.manufacturer_name() {
            write!(f, "0x{:08X} ({})", self.0, mfn)
        } else {
            write!(f, "0x{:08X}", self.0)
        }
    }
}

impl IdCode {
    /// Returns `true` iff the IDCODE's least significant bit is `1`
    /// and the 7-bit `manufacturer_identity` is set to one of the non-reserved values in the range `[1,126]`.
    pub fn valid(&self) -> bool {
        self.lsbit() && (self.manufacturer() != 0) && (self.manufacturer() != 127)
    }

    /// Return the manufacturer name, if available.
    pub fn manufacturer_name(&self) -> Option<&'static str> {
        let cc = self.manufacturer_continuation();
        let id = self.manufacturer_identity();
        jep106::JEP106Code::new(cc, id).get()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanChainError {
    #[error("Invalid IDCODE")]
    InvalidIdCode,
    #[error("Invalid IR scan chain")]
    InvalidIR,
}

/// Convert a list of start positions to a list of lengths.
fn starts_to_lengths(starts: &[usize], total: usize) -> Vec<usize> {
    let mut lens: Vec<usize> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    lens.push(total - lens.iter().sum::<usize>());
    lens
}

/// Extract all IDCODEs from a test-logic-reset DR chain `dr`.
///
/// Valid IDCODEs have a '1' in the least significant (first) bit,
/// and are 32 bits long. DRs in BYPASS always have a single 0 bit.
///
/// We can therefore unambiguously scan through the DR capture to find
/// all IDCODEs and TAPs in BYPASS.
///
/// Because we don't know how many TAPs there are, we scan until we find
/// a 32-bit IDCODE of all 1s, which comes after the last TAP in the chain.
///
/// Returns Vec<Option<IdCode>>, with None for TAPs in BYPASS.
pub(crate) fn extract_idcodes(
    mut dr: &BitSlice<u8>,
) -> Result<Vec<Option<IdCode>>, ScanChainError> {
    let mut idcodes = Vec::new();

    while !dr.is_empty() {
        if dr[0] {
            if dr.len() < 32 {
                tracing::error!("Truncated IDCODE: {dr:02X?}");
                return Err(ScanChainError::InvalidIdCode);
            }

            let idcode = dr[0..32].load_le::<u32>();

            if idcode == u32::MAX {
                break;
            }

            let idcode = IdCode(idcode);

            if !idcode.valid() {
                tracing::error!("Invalid IDCODE: {:08X}", idcode.0);
                return Err(ScanChainError::InvalidIdCode);
            }
            tracing::info!("Found IDCODE: {idcode}");
            idcodes.push(Some(idcode));
            dr = &dr[32..];
        } else {
            idcodes.push(None);
            tracing::info!("Found bypass TAP");
            dr = &dr[1..];
        }
    }
    Ok(idcodes)
}

pub(crate) fn common_sequence<'a, S: BitStore>(
    a: &'a BitSlice<S>,
    b: &BitSlice<S>,
) -> &'a BitSlice<S> {
    let common_length = a.iter().zip(b.iter()).take_while(|(a, b)| *a == *b).count();

    &a[..common_length]
}

/// Best-effort extraction of IR lengths from a test-logic-reset IR chain `ir`,
/// which is known to contain `n_taps` TAPs (as discovered by scanning DR for IDCODEs).
///
/// If expected IR lengths are provided, specify them in `expected`, and they are
/// verified against the IR scan and then returned.
///
/// Valid IRs in the capture must start with `0b10` (a 1 in the least-significant,
/// and therefore first, bit). However, IRs may contain `0b10` in other positions, so we
/// can only find a superset of all possible start positions. If this happens to match
/// the number of taps, or there is only one tap, we can find all IR lengths. Otherwise,
/// they must be provided, and are then checked.
///
/// This implementation is a port of the algorithm from:
/// https://github.com/GlasgowEmbedded/glasgow/blob/30dc11b2/software/glasgow/applet/interface/jtag_probe/__init__.py#L712
///
/// Returns Vec<usize>, with an entry for each TAP.
pub(crate) fn extract_ir_lengths(
    ir: &BitSlice<u8>,
    n_taps: usize,
    expected: Option<&[usize]>,
) -> Result<Vec<usize>, ScanChainError> {
    // Find all `10` patterns which indicate potential IR start positions.
    let starts = ir
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] && !w[1])
        .map(|(i, _)| i)
        .collect::<Vec<usize>>();
    tracing::trace!("Possible IR start positions: {starts:?}");

    if n_taps == 0 {
        tracing::error!("Cannot scan IR without at least one TAP");
        Err(ScanChainError::InvalidIR)
    } else if n_taps > starts.len() {
        // We must have at least as many `10` patterns as TAPs.
        tracing::error!("Fewer IRs detected than TAPs");
        Err(ScanChainError::InvalidIR)
    } else if starts[0] != 0 {
        // The chain must begin with a possible start location.
        tracing::error!("IR chain does not begin with a valid start pattern");
        Err(ScanChainError::InvalidIR)
    } else if let Some(expected) = expected {
        // If expected lengths are available, verify and return them.
        if expected.len() != n_taps {
            tracing::error!(
                "Number of provided IR lengths ({}) does not match \
                         number of detected TAPs ({n_taps})",
                expected.len()
            );

            Err(ScanChainError::InvalidIR)
        } else if expected.iter().sum::<usize>() != ir.len() {
            tracing::error!(
                "Sum of provided IR lengths ({}) does not match \
                         length of IR scan ({} bits)",
                expected.iter().sum::<usize>(),
                ir.len()
            );
            Err(ScanChainError::InvalidIR)
        } else {
            let exp_starts = expected
                .iter()
                .scan(0, |a, &x| {
                    let b = *a;
                    *a += x;
                    Some(b)
                })
                .collect::<Vec<usize>>();
            tracing::trace!("Provided IR start positions: {exp_starts:?}");
            let unsupported = exp_starts.iter().filter(|s| !starts.contains(s)).count();
            if unsupported > 0 {
                tracing::error!(
                    "Provided IR lengths imply an IR start position \
                             which is not supported by the IR scan"
                );
                Err(ScanChainError::InvalidIR)
            } else {
                tracing::debug!("Verified provided IR lengths against IR scan");
                Ok(starts_to_lengths(&exp_starts, ir.len()))
            }
        }
    } else if n_taps == 1 {
        // If there's only one TAP, this is easy.
        tracing::info!("Only one TAP detected, IR length {}", ir.len());
        Ok(vec![ir.len()])
    } else if n_taps == starts.len() {
        // If the number of possible starts matches the number of TAPs,
        // we can unambiguously find all lengths.
        let irlens = starts_to_lengths(&starts, ir.len());
        tracing::info!("IR lengths are unambiguous: {irlens:?}");
        Ok(irlens)
    } else {
        tracing::error!("IR lengths are ambiguous and must be explicitly configured.");
        Err(ScanChainError::InvalidIR)
    }
}

/// Inner states of the parallel arms (IR-Scan and DR-Scan) of the JTAG state machine.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RegisterState {
    Select,
    Capture,
    Shift,
    Exit1,
    Pause,
    Exit2,
    Update,
}

impl RegisterState {
    fn step_toward(self, target: Self) -> bool {
        match self {
            Self::Select => false,
            Self::Capture if matches!(target, Self::Shift) => false,
            Self::Exit1 if matches!(target, Self::Pause | Self::Exit2) => false,
            Self::Exit2 if matches!(target, Self::Shift | Self::Exit1 | Self::Pause) => false,
            Self::Update => {
                unreachable!("This is a bug, this case should have been handled by JtagState.")
            }
            _ => true,
        }
    }

    fn update(self, tms: bool) -> Self {
        if tms {
            match self {
                Self::Capture | Self::Shift => Self::Exit1,
                Self::Exit1 | Self::Exit2 => Self::Update,
                Self::Pause => Self::Exit2,
                Self::Select | Self::Update => {
                    unreachable!("This is a bug, this case should have been handled by JtagState.")
                }
            }
        } else {
            match self {
                Self::Select => Self::Capture,
                Self::Capture | Self::Shift => Self::Shift,
                Self::Exit1 | Self::Pause => Self::Pause,
                Self::Exit2 => Self::Shift,
                Self::Update => {
                    unreachable!("This is a bug, this case should have been handled by JtagState.")
                }
            }
        }
    }
}

/// JTAG State Machine representation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum JtagState {
    Reset,
    Idle,
    Dr(RegisterState),
    Ir(RegisterState),
}

impl JtagState {
    /// Returns the TMS value that takes a step from the current state toward the target state.
    ///
    /// Returns `None` if the state machine is already in the target state.
    pub fn step_toward(self, target: Self) -> Option<bool> {
        let tms = match self {
            state if target == state => return None,
            Self::Reset => false,
            Self::Idle => true,
            Self::Dr(RegisterState::Select) => !matches!(target, Self::Dr(_)),
            Self::Ir(RegisterState::Select) => !matches!(target, Self::Ir(_)),
            Self::Dr(RegisterState::Update) | Self::Ir(RegisterState::Update) => {
                matches!(target, Self::Ir(_) | Self::Dr(_))
            }
            Self::Dr(state) => {
                // Decide if we need to stay in the current arm or not.
                // The inner state machine will handle the case where we need to loop back
                // through Run-Test/Idle.
                let next = if let Self::Dr(target) = target {
                    target
                } else {
                    // Let's aim for the inner state that can exit the scan arm.
                    RegisterState::Update
                };
                state.step_toward(next)
            }
            Self::Ir(state) => {
                // Decide if we need to stay in the current arm or not.
                // The inner state machine will handle the case where we need to loop back
                // through Run-Test/Idle.
                let next = if let Self::Ir(target) = target {
                    target
                } else {
                    // Let's aim for the inner state that can exit the scan arm.
                    RegisterState::Update
                };
                state.step_toward(next)
            }
        };
        Some(tms)
    }

    /// Updates the state machine from the given TMS bit.
    pub fn update(&mut self, tms: bool) {
        *self = match *self {
            Self::Reset if tms => Self::Reset,
            Self::Reset => Self::Idle,
            Self::Idle if tms => Self::Dr(RegisterState::Select),
            Self::Idle => Self::Idle,
            Self::Dr(RegisterState::Select) if tms => Self::Ir(RegisterState::Select),
            Self::Ir(RegisterState::Select) if tms => Self::Reset,
            Self::Dr(RegisterState::Update) | Self::Ir(RegisterState::Update) => {
                if tms {
                    Self::Dr(RegisterState::Select)
                } else {
                    Self::Idle
                }
            }
            Self::Dr(state) => Self::Dr(state.update(tms)),
            Self::Ir(state) => Self::Ir(state.update(tms)),
        };
    }
}

#[derive(Debug)]
pub(crate) struct JtagDriverState {
    pub state: JtagState,
    pub current_ir_reg: u32,
    // The maximum IR address
    pub max_ir_address: u32,
    pub expected_scan_chain: Option<Vec<ScanChainElement>>,
    pub scan_chain: Vec<JtagChainItem>,
    pub chain_params: ChainParams,
    /// Idle cycles necessary between consecutive
    /// accesses to the DMI register
    pub jtag_idle_cycles: usize,
}

impl Default for JtagDriverState {
    fn default() -> Self {
        Self {
            state: JtagState::Reset,
            current_ir_reg: 1,
            max_ir_address: 0x0F,
            expected_scan_chain: None,
            scan_chain: Vec::new(),
            chain_params: ChainParams::default(),
            jtag_idle_cycles: 0,
        }
    }
}

/// A trait for implementing low-level JTAG interface operations.
pub(crate) trait RawJtagIo {
    /// Returns a mutable reference to the current state.
    fn state_mut(&mut self) -> &mut JtagDriverState;

    /// Returns the current state.
    fn state(&self) -> &JtagDriverState;

    /// Shifts a number of bits through the TAP.
    fn shift_bits(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: impl IntoIterator<Item = bool>,
    ) -> Result<(), DebugProbeError> {
        for ((tms, tdi), cap) in tms.into_iter().zip(tdi.into_iter()).zip(cap.into_iter()) {
            self.shift_bit(tms, tdi, cap)?;
        }

        Ok(())
    }

    /// Shifts a single bit through the TAP.
    ///
    /// Drivers may choose, and are encouraged, to buffer bits and flush them
    /// in batches for performance reasons.
    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError>;

    /// Returns the bits captured from TDO and clears the capture buffer.
    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError>;

    /// Resets the JTAG state machine by shifting out a number of high TMS bits.
    fn reset_jtag_state_machine(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Resetting JTAG chain by setting tms high for 5 bits");

        // Reset JTAG chain (5 times TMS high), and enter idle state afterwards
        let tms = [true, true, true, true, true, false];
        let tdi = iter::repeat(true);

        self.shift_bits(tms, tdi, iter::repeat(false))?;
        let response = self.read_captured_bits()?;

        tracing::debug!("Response to reset: {}", response);

        Ok(())
    }

    /// Configures the probe to address the given target.
    fn select_target(&mut self, target: usize) -> Result<(), DebugProbeError> {
        let state = self.state_mut();

        let Some(params) = ChainParams::from_jtag_chain(&state.scan_chain, target) else {
            return Err(DebugProbeError::TargetNotFound);
        };

        let max_ir_address = (1 << params.irlen) - 1;

        tracing::debug!("Setting chain params: {:?}", params);
        tracing::debug!("Setting max_ir_address to {}", max_ir_address);

        let state = self.state_mut();
        state.max_ir_address = max_ir_address;
        state.chain_params = params;

        Ok(())
    }
}

fn jtag_move_to_state(
    protocol: &mut impl RawJtagIo,
    target: JtagState,
) -> Result<(), DebugProbeError> {
    tracing::trace!(
        "Changing state: {:?} -> {:?}",
        protocol.state_mut().state,
        target
    );

    while let Some(tms) = protocol.state().state.step_toward(target) {
        protocol.shift_bit(tms, false, false)?;
    }

    tracing::trace!("In state: {:?}", protocol.state_mut().state);
    Ok(())
}

fn shift_ir(
    protocol: &mut impl RawJtagIo,
    data: &[u8],
    len: usize,
    capture_data: bool,
) -> Result<(), DebugProbeError> {
    tracing::debug!("Write IR: {:?}, len={}", data, len);

    // Check the bit length, enough data has to be available
    if data.len() * 8 < len || len == 0 {
        return Err(DebugProbeError::Other(anyhow!(
            "Invalid data length. IR bits: {}, expected: {}",
            data.len(),
            len
        )));
    }

    // BYPASS commands before and after shifting out data where required
    let pre_bits = protocol.state().chain_params.irpre;
    let post_bits = protocol.state().chain_params.irpost;

    // The last bit will be transmitted when exiting the shift state,
    // so we need to stay in the shift state for one period less than
    // we have bits to transmit.
    let tms_data = iter::repeat(false).take(len - 1);

    // Enter IR shift
    jtag_move_to_state(protocol, JtagState::Ir(RegisterState::Shift))?;

    let tms = iter::repeat(false)
        .take(pre_bits)
        .chain(tms_data)
        .chain(iter::repeat(false).take(post_bits))
        .chain(iter::once(true));

    let tdi = iter::repeat(true)
        .take(pre_bits)
        .chain(data.as_bits::<Lsb0>()[..len].iter().map(|b| *b))
        .chain(iter::repeat(true).take(post_bits));

    let capture = iter::repeat(false)
        .take(pre_bits)
        .chain(iter::repeat(capture_data).take(len))
        .chain(iter::repeat(false));

    tracing::trace!("tms: {:?}", tms.clone());
    tracing::trace!("tdi: {:?}", tdi.clone());

    protocol.shift_bits(tms, tdi, capture)?;
    jtag_move_to_state(protocol, JtagState::Ir(RegisterState::Update))?;

    Ok(())
}

fn shift_dr(
    protocol: &mut impl RawJtagIo,
    data: &[u8],
    register_bits: usize,
    capture_data: bool,
) -> Result<usize, DebugProbeError> {
    tracing::debug!("Write DR: {:?}, len={}", data, register_bits);

    // Check the bit length, enough data has to be available
    if data.len() * 8 < register_bits || register_bits == 0 {
        return Err(DebugProbeError::Other(anyhow!(
            "Invalid data length. DR bits: {}, expected: {}",
            data.len(),
            register_bits
        )));
    }

    // Last bit of data is shifted out when we exit the SHIFT-DR State
    let tms_shift_out_value = iter::repeat(false).take(register_bits - 1);

    // Enter DR shift
    jtag_move_to_state(protocol, JtagState::Dr(RegisterState::Shift))?;

    // dummy bits to account for bypasses
    let pre_bits = protocol.state().chain_params.drpre;
    let post_bits = protocol.state().chain_params.drpost;

    let tms = iter::repeat(false)
        .take(pre_bits)
        .chain(tms_shift_out_value)
        .chain(iter::repeat(false).take(post_bits))
        .chain(iter::once(true));

    let tdi = iter::repeat(false)
        .take(pre_bits)
        .chain(data.as_bits::<Lsb0>()[..register_bits].iter().map(|b| *b))
        .chain(iter::repeat(false).take(post_bits));

    let capture = iter::repeat(false)
        .take(pre_bits)
        .chain(iter::repeat(capture_data).take(register_bits))
        .chain(iter::repeat(false));

    protocol.shift_bits(tms, tdi, capture)?;

    jtag_move_to_state(protocol, JtagState::Dr(RegisterState::Update))?;

    let idle_cycles = protocol.state().jtag_idle_cycles;
    if idle_cycles > 0 {
        jtag_move_to_state(protocol, JtagState::Idle)?;

        // We need to stay in the idle cycle a bit
        let tms = iter::repeat(false).take(idle_cycles);
        let tdi = iter::repeat(false).take(idle_cycles);

        protocol.shift_bits(tms, tdi, iter::repeat(false))?;
    }

    if capture_data {
        Ok(register_bits)
    } else {
        Ok(0)
    }
}

fn prepare_write_register(
    protocol: &mut impl RawJtagIo,
    address: u32,
    data: &[u8],
    len: u32,
    capture: bool,
) -> Result<usize, DebugProbeError> {
    if protocol.state().current_ir_reg != address {
        if address > protocol.state().max_ir_address {
            return Err(DebugProbeError::Other(anyhow!(
                "Invalid instruction register access: {}",
                address
            )));
        }

        let ir_len = protocol.state().chain_params.irlen;
        shift_ir(protocol, &address.to_le_bytes(), ir_len, false)?;
        protocol.state_mut().current_ir_reg = address;
    }

    // read DR register by transfering len bits to the chain
    shift_dr(protocol, data, len as usize, capture)
}

impl<Probe: DebugProbe + RawJtagIo + 'static> JTAGAccess for Probe {
    fn scan_chain(&mut self) -> Result<(), DebugProbeError> {
        const MAX_CHAIN: usize = 8;

        self.reset_jtag_state_machine()?;

        self.state_mut().chain_params = ChainParams::default();

        let input = vec![0xFF; 4 * MAX_CHAIN];

        shift_dr(self, &input, input.len() * 8, true)?;
        let response = self.read_captured_bits()?;

        tracing::debug!("DR: {:?}", response);

        let idcodes = extract_idcodes(&response).map_err(|e| DebugProbeError::Other(e.into()))?;

        tracing::info!(
            "JTAG DR scan complete, found {} TAPs. {:?}",
            idcodes.len(),
            idcodes
        );

        tracing::debug!("Scanning JTAG chain for IR lengths");

        // First shift out all ones
        let input = vec![0xff; idcodes.len()];
        shift_ir(self, &input, input.len() * 8, true)?;
        let response = self.read_captured_bits()?;

        tracing::debug!("IR scan: {}", response);

        self.reset_jtag_state_machine()?;

        // Next, shift out same amount of zeros, then ones to make sure the IRs contain BYPASS.
        let input = iter::repeat(0)
            .take(idcodes.len())
            .chain(input.iter().copied())
            .collect::<Vec<_>>();
        shift_ir(self, &input, input.len() * 8, true)?;
        let response_zeros = self.read_captured_bits()?;

        tracing::debug!("IR scan: {}", response_zeros);

        let response = response.as_bitslice();
        let response = common_sequence(response, response_zeros.as_bitslice());

        tracing::debug!("IR scan: {}", response);

        let ir_lens = extract_ir_lengths(
            response,
            idcodes.len(),
            self.state()
                .expected_scan_chain
                .as_ref()
                .map(|chain| {
                    chain
                        .iter()
                        .filter_map(|s| s.ir_len)
                        .map(|s| s as usize)
                        .collect::<Vec<usize>>()
                })
                .as_deref(),
        )
        .map_err(|e| DebugProbeError::Other(e.into()))?;

        tracing::info!("Found {} TAPs on reset scan", idcodes.len());
        tracing::debug!("Detected IR lens: {:?}", ir_lens);

        let chain = idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| JtagChainItem { irlen, idcode })
            .collect::<Vec<_>>();

        self.state_mut().scan_chain = chain;

        Ok(())
    }

    fn tap_reset(&mut self) -> Result<(), DebugProbeError> {
        self.reset_jtag_state_machine()
    }

    fn set_idle_cycles(&mut self, idle_cycles: u8) {
        self.state_mut().jtag_idle_cycles = idle_cycles as usize;
    }

    fn idle_cycles(&self) -> u8 {
        self.state().jtag_idle_cycles as u8
    }

    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        let data = vec![0u8; (len as usize + 7) / 8];

        self.write_register(address, &data, len)
    }

    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError> {
        prepare_write_register(self, address, data, len, true)?;

        let mut response = self.read_captured_bits()?;

        // Implementations don't need to align to keep the code simple
        response.force_align();
        let result = response.into_vec();

        tracing::trace!("recieve_write_dr result: {:?}", result);
        Ok(result)
    }

    #[tracing::instrument(skip(self, writes))]
    fn write_register_batch(
        &mut self,
        writes: &JtagCommandQueue,
    ) -> Result<DeferredResultSet, BatchExecutionError> {
        let mut bits = Vec::with_capacity(writes.len());
        let t1 = std::time::Instant::now();
        tracing::debug!("Preparing {} writes...", writes.len());
        for (idx, write) in writes.iter() {
            // If an error happens during prep, return no results as chip will be in an inconsistent state
            let op = prepare_write_register(
                self,
                write.address,
                &write.data,
                write.len,
                idx.should_capture(),
            )
            .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

            bits.push((idx, write, op));
        }

        tracing::debug!("Sending to chip...");
        // If an error happens during the final flush, also retry whole operation
        let bitstream = self
            .read_captured_bits()
            .map_err(|e| BatchExecutionError::new(e.into(), DeferredResultSet::new()))?;

        tracing::debug!("Got responses! Took {:?}! Processing...", t1.elapsed());
        let mut responses = DeferredResultSet::with_capacity(bits.len());

        let mut bitstream = bitstream.as_bitslice();
        for (idx, command, bits) in bits.into_iter() {
            if idx.should_capture() {
                // TODO: this back-and-forth between BitVec and Vec is probably unnecessary
                let mut reg_bits = bitstream[..bits].to_bitvec();
                reg_bits.force_align();
                let response = reg_bits.into_vec();
                match (command.transform)(command, response) {
                    Ok(response) => responses.push(idx, response),
                    Err(e) => return Err(BatchExecutionError::new(e, responses)),
                }
            }

            bitstream = &bitstream[bits..];
        }

        Ok(responses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARM_TAP: IdCode = IdCode(0x4BA00477);
    const STM_BS_TAP: IdCode = IdCode(0x06433041);

    #[test]
    fn id_code_display() {
        let debug_fmt = format!("{idcode}", idcode = ARM_TAP);
        assert_eq!(debug_fmt, "0x4BA00477 (ARM Ltd)");

        let debug_fmt = format!("{idcode}", idcode = STM_BS_TAP);
        assert_eq!(debug_fmt, "0x06433041 (STMicroelectronics)");
    }

    #[test]
    fn extract_ir_lengths_with_one_tap() {
        let ir = &bitvec![u8, Lsb0; 1,0,0,0];
        let n_taps = 1;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4]);
    }

    #[test]
    fn extract_ir_lengths_with_two_taps() {
        // The STM32F1xx and STM32F4xx are examples of MCUs that two serially connected JTAG TAPs,
        // the boundary scan TAP (IR is 5-bit wide) and the Cortex® -M4 with FPU TAP (IR is 4-bit wide).
        // This test ensures our scan chain interrogation handles this scenario.
        let ir = &bitvec![u8, Lsb0; 1,0,0,0,1,0,0,0,0];
        let n_taps = 2;
        let expected = None;

        let ir_lengths = extract_ir_lengths(ir, n_taps, expected).unwrap();

        assert_eq!(ir_lengths, vec![4, 5]);
    }

    #[test]
    fn extract_id_codes_one_tap() {
        let mut dr = bitvec![u8, Lsb0; 0; 32];
        dr[0..32].store_le(ARM_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP)]);
    }

    #[test]
    fn extract_id_codes_two_taps() {
        let mut dr = bitvec![u8, Lsb0; 0; 64];
        dr[0..32].store_le(ARM_TAP.0);
        dr[32..64].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), Some(STM_BS_TAP)]);
    }

    #[test]
    fn extract_id_codes_tap_bypass_tap() {
        let mut dr = bitvec![u8, Lsb0; 0; 65];
        dr[0..32].store_le(ARM_TAP.0);
        dr.set(32, false);
        dr[33..65].store_le(STM_BS_TAP.0);

        let idcodes = extract_idcodes(&dr).unwrap();

        assert_eq!(idcodes, vec![Some(ARM_TAP), None, Some(STM_BS_TAP)]);
    }

    #[test]
    fn reset_from_ir_shift() {
        let mut state = JtagState::Ir(RegisterState::Shift);
        state.update(true);
        state.update(true);
        state.update(true);
        state.update(true);
        state.update(true);
        assert_eq!(state, JtagState::Reset);
    }

    #[test]
    fn idle_from_reset() {
        let mut state = JtagState::Reset;
        state.update(false);
        assert_eq!(state, JtagState::Idle);
    }

    #[test]
    fn generated_bits_lead_to_correct_state() {
        for (start, goal) in [(JtagState::Reset, JtagState::Idle)] {
            let mut state = start;
            let mut transitions = 0;
            while state != goal && transitions < 10 {
                let tms = state.step_toward(goal).unwrap();
                state.update(tms);
                transitions += 1;
            }

            assert!(transitions < 10);
        }
    }
}
