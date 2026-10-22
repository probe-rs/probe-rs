//! Bus access for TriCore debug probes.
//!
//! TriCore debug works entirely through memory-mapped registers: the OCDS registers of every
//! core sit in its CSFR window, and the core registers are mapped there too. A probe therefore
//! only needs to read and write memory and to reset the device. [`TriCoreBusAccess`] is that
//! interface. Probes that talk to a debug server (Infineon TAS) or to a vendor library (MCD)
//! implement it, and the TriCore core implementation is shared by all of them.

use crate::probe::DebugProbeError;

/// An address map (address space) that a transfer targets.
///
/// The numbers are the ones used by the Infineon Tool Access Socket (TAS) protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AddressMap {
    /// The device address space (map 0).
    Target = 0,
    /// The emulation extension chip address space, which holds the MCDS registers (map 1).
    Eec = 1,
    /// The debug server's own registers (map 15), for example the MCDS timestamp.
    Server = 15,
}

impl std::fmt::Display for AddressMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Target => "0 (device)",
            Self::Eec => "1 (EEC)",
            Self::Server => "15 (debug server)",
        })
    }
}

/// The direction and data of a [`BusTransfer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferKind {
    /// Read `len` bytes. The data is stored in [`BusTransfer::data`].
    Read {
        /// The number of bytes to read.
        len: usize,
    },
    /// Write the bytes in [`BusTransfer::data`].
    Write,
}

/// One memory transfer of a [`TriCoreBusAccess::execute`] batch.
///
/// A transfer must be one access the probe can do in one step. Its length is 1, 2, 4 or 8 bytes,
/// aligned to its size, or an 8-byte aligned block of a multiple of 8 bytes and at most
/// [`BusLimits::max_block_size`] bytes. [`TriCoreBusAccess::read_block`] and
/// [`TriCoreBusAccess::write_block`] split any access into such transfers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusTransfer {
    /// The address map of the transfer.
    pub map: AddressMap,
    /// The address of the first byte.
    pub address: u32,
    /// Read or write.
    pub kind: TransferKind,
    /// The data written, or the data read once the transfer has completed.
    pub data: Vec<u8>,
    /// The status code that the probe reported for this transfer, once it has completed.
    pub status: Option<TransferStatus>,
}

/// The result of one [`BusTransfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// The transfer succeeded.
    Ok,
    /// The probe or the device refused the access, with the probe-specific error code.
    Failed(u8),
}

impl BusTransfer {
    /// A read of `len` bytes at `address`.
    pub fn read(map: AddressMap, address: u32, len: usize) -> Self {
        Self {
            map,
            address,
            kind: TransferKind::Read { len },
            data: Vec::new(),
            status: None,
        }
    }

    /// A write of `data` at `address`.
    pub fn write(map: AddressMap, address: u32, data: impl Into<Vec<u8>>) -> Self {
        Self {
            map,
            address,
            kind: TransferKind::Write,
            data: data.into(),
            status: None,
        }
    }

    /// A write of one 32-bit word at `address`.
    pub fn write_u32(map: AddressMap, address: u32, value: u32) -> Self {
        Self::write(map, address, value.to_le_bytes())
    }

    /// The number of bytes this transfer moves.
    pub fn len(&self) -> usize {
        match self.kind {
            TransferKind::Read { len } => len,
            TransferKind::Write => self.data.len(),
        }
    }

    /// Whether the transfer moves no data.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this is a read.
    pub fn is_read(&self) -> bool {
        matches!(self.kind, TransferKind::Read { .. })
    }

    /// Returns an error if the transfer did not complete successfully.
    pub fn check(&self) -> Result<(), TriCoreError> {
        match self.status {
            Some(TransferStatus::Ok) => Ok(()),
            Some(TransferStatus::Failed(code)) => Err(TriCoreError::TransferFailed {
                map: self.map,
                address: self.address,
                len: self.len(),
                code,
            }),
            None => Err(TriCoreError::TransferNotExecuted {
                map: self.map,
                address: self.address,
            }),
        }
    }

    /// The first four bytes read, as a little-endian word.
    pub fn read_u32(&self) -> Result<u32, TriCoreError> {
        self.check()?;
        let bytes: [u8; 4] =
            self.data
                .get(..4)
                .and_then(|b| b.try_into().ok())
                .ok_or(TriCoreError::Protocol(format!(
                    "read at {:#010x} returned {} bytes, expected 4",
                    self.address,
                    self.data.len()
                )))?;
        Ok(u32::from_le_bytes(bytes))
    }
}

/// How many transfers and bytes a probe can handle in one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusLimits {
    /// The maximum number of transfers in one [`TriCoreBusAccess::execute`] call that the probe
    /// sends as one request. Longer batches are split.
    pub max_transfers: usize,
    /// The maximum length of one block transfer in bytes.
    pub max_block_size: usize,
}

impl Default for BusLimits {
    fn default() -> Self {
        // The TAS limits: 32 transactions per PL0 frame, 1 KiB per block.
        Self {
            max_transfers: 32,
            max_block_size: 1024,
        }
    }
}

/// The kinds of device reset a [`TriCoreBusAccess`] can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    /// Reset the device and keep the cores halted after the reset, with debug enabled.
    ResetAndHalt,
}

/// Memory and reset access to a TriCore device, provided by a debug probe.
pub trait TriCoreBusAccess: Send + std::fmt::Debug {
    /// Executes the transfers in order, and stores each result in [`BusTransfer::status`] (and
    /// the read data in [`BusTransfer::data`]).
    ///
    /// A failing transfer is reported in its status, not as an `Err`. `Err` means that the
    /// probe could not run the batch at all, for example because the connection was lost. The
    /// probe stops at the first failing transfer; the transfers after it have no status.
    fn execute(&mut self, transfers: &mut [BusTransfer]) -> Result<(), TriCoreError>;

    /// The transfer limits of the probe.
    fn limits(&self) -> BusLimits {
        BusLimits::default()
    }

    /// Resets the device.
    fn reset(&mut self, kind: ResetKind) -> Result<(), TriCoreError>;

    /// The JTAG ID code of the device.
    fn jtag_id(&mut self) -> Result<u32, TriCoreError>;

    /// Reads `buf.len()` bytes at `address`, splitting the access into transfers the probe
    /// supports.
    fn read_block(
        &mut self,
        map: AddressMap,
        address: u32,
        buf: &mut [u8],
    ) -> Result<(), TriCoreError> {
        let limits = self.limits();
        let mut transfers = split_access(address, buf.len(), limits)
            .into_iter()
            .map(|(addr, len)| BusTransfer::read(map, addr, len))
            .collect::<Vec<_>>();
        execute_all(self, &mut transfers, limits)?;
        let mut offset = 0;
        for transfer in &transfers {
            transfer.check()?;
            let len = transfer.len();
            let data = transfer.data.get(..len).ok_or_else(|| {
                TriCoreError::Protocol(format!(
                    "read at {:#010x} returned {} bytes, expected {len}",
                    transfer.address,
                    transfer.data.len()
                ))
            })?;
            buf[offset..offset + len].copy_from_slice(data);
            offset += len;
        }
        Ok(())
    }

    /// Writes `data` at `address`, splitting the access into transfers the probe supports.
    fn write_block(
        &mut self,
        map: AddressMap,
        address: u32,
        data: &[u8],
    ) -> Result<(), TriCoreError> {
        let limits = self.limits();
        let mut offset = 0;
        let mut transfers = split_access(address, data.len(), limits)
            .into_iter()
            .map(|(addr, len)| {
                let transfer = BusTransfer::write(map, addr, &data[offset..offset + len]);
                offset += len;
                transfer
            })
            .collect::<Vec<_>>();
        execute_all(self, &mut transfers, limits)?;
        transfers.iter().try_for_each(BusTransfer::check)
    }

    /// Reads one 32-bit word.
    fn read_u32(&mut self, map: AddressMap, address: u32) -> Result<u32, TriCoreError> {
        let mut transfer = [BusTransfer::read(map, address, 4)];
        self.execute(&mut transfer)?;
        transfer[0].read_u32()
    }

    /// Writes one 32-bit word.
    fn write_u32(&mut self, map: AddressMap, address: u32, value: u32) -> Result<(), TriCoreError> {
        let mut transfer = [BusTransfer::write_u32(map, address, value)];
        self.execute(&mut transfer)?;
        transfer[0].check()
    }
}

/// Runs `transfers` in batches of at most `limits.max_transfers`.
fn execute_all<B: TriCoreBusAccess + ?Sized>(
    bus: &mut B,
    transfers: &mut [BusTransfer],
    limits: BusLimits,
) -> Result<(), TriCoreError> {
    for chunk in transfers.chunks_mut(limits.max_transfers.max(1)) {
        bus.execute(chunk)?;
        // Stop at the first failure, the caller reports it.
        if chunk.iter().any(|t| t.status != Some(TransferStatus::Ok)) {
            break;
        }
    }
    Ok(())
}

/// Splits an access of `len` bytes at `address` into `(address, len)` transfers that follow the
/// rules of [`BusTransfer`]. The device requires natural alignment, so unaligned heads and tails
/// use smaller transfers.
pub(crate) fn split_access(
    mut address: u32,
    mut len: usize,
    limits: BusLimits,
) -> Vec<(u32, usize)> {
    let max_block = (limits.max_block_size / 8 * 8).max(8);
    let mut out = Vec::new();
    while len > 0 {
        let n = if address.is_multiple_of(8) && len >= 8 {
            (len / 8 * 8).min(max_block)
        } else if address.is_multiple_of(4) && len >= 4 {
            4
        } else if address.is_multiple_of(2) && len >= 2 {
            2
        } else {
            1
        };
        out.push((address, n));
        address = address.wrapping_add(n as u32);
        len -= n;
    }
    out
}

/// TriCore specific errors.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum TriCoreError {
    /// An error originating from the debug probe occurred.
    DebugProbe(#[from] DebugProbeError),

    /// The probe reported error {code:#04x} for {len} bytes at {address:#010x} in address map {map}.
    TransferFailed {
        /// The address map of the transfer.
        map: AddressMap,
        /// The address of the transfer.
        address: u32,
        /// The length of the transfer.
        len: usize,
        /// The probe specific error code.
        code: u8,
    },

    /// The transfer at {address:#010x} in address map {map} was not executed.
    TransferNotExecuted {
        /// The address map of the transfer.
        map: AddressMap,
        /// The address of the transfer.
        address: u32,
    },

    /// The debug probe protocol was violated: {0}
    Protocol(String),

    /// The connected target is not a TriCore device.
    NoTriCoreTarget,

    /// Core {core} did not halt (DBGSR = {dbgsr:#010x}).
    CoreDidNotHalt {
        /// The core index.
        core: u8,
        /// The last DBGSR value.
        dbgsr: u32,
    },

    /// Core {core} did not start running (DBGSR = {dbgsr:#010x}).
    CoreDidNotRun {
        /// The core index.
        core: u8,
        /// The last DBGSR value.
        dbgsr: u32,
    },

    /// Core {0} did not complete the single step.
    StepFailed(u8),

    /// Core {0} must be halted for this operation.
    NotHalted(u8),

    /// Breakpoint unit {0} does not exist.
    BreakpointOutOfBounds(usize),

    /// The operation has timed out.
    Timeout,
}

impl From<TriCoreError> for crate::Error {
    fn from(err: TriCoreError) -> Self {
        match err {
            TriCoreError::DebugProbe(e) => e.into(),
            TriCoreError::Timeout => crate::Error::Timeout,
            other => crate::Error::TriCore(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> BusLimits {
        BusLimits::default()
    }

    #[test]
    fn aligned_access_is_one_block() {
        assert_eq!(
            split_access(0x7000_0000, 64, limits()),
            vec![(0x7000_0000, 64)]
        );
    }

    #[test]
    fn long_access_is_split_into_kib_blocks() {
        assert_eq!(
            split_access(0x7000_0000, 2500, limits()),
            vec![
                (0x7000_0000, 1024),
                (0x7000_0400, 1024),
                (0x7000_0800, 448),
                (0x7000_09C0, 4),
            ]
        );
    }

    #[test]
    fn unaligned_access_uses_small_transfers() {
        assert_eq!(
            split_access(0x7000_0003, 14, limits()),
            vec![
                (0x7000_0003, 1),
                (0x7000_0004, 4),
                (0x7000_0008, 8),
                (0x7000_0010, 1),
            ]
        );
    }

    #[test]
    fn blocks_start_on_eight_byte_boundaries() {
        assert_eq!(
            split_access(0xF881_F004, 12, limits()),
            vec![(0xF881_F004, 4), (0xF881_F008, 8)]
        );
        assert_eq!(
            split_access(0x7000_0002, 4, limits()),
            vec![(0x7000_0002, 2), (0x7000_0004, 2)]
        );
    }
}
