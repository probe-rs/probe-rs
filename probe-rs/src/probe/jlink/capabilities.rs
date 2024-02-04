//! Probe capabilities.

#![allow(non_upper_case_globals)]

use std::fmt;

enum_and_set!(
    /// List of capabilities that may be advertised by a probe.
    ///
    /// Not many of these are actually used, and a lot of these have unknown meaning.
    #[non_exhaustive]
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub enum Capability {
        Reserved0 = 0,  // Reserved, seems to be always set
        GetHwVersion = 1,
        WriteDcc = 2,
        AdaptiveClocking = 3,
        ReadConfig = 4,
        WriteConfig = 5,
        Trace = 6,
        WriteMem = 7,
        ReadMem = 8,
        SpeedInfo = 9,
        ExecCode = 10,
        GetMaxBlockSize = 11,
        GetHwInfo = 12,
        SetKsPower = 13,
        ResetStopTimed = 14,
        // 15 = Reserved, seems to never be set
        MeasureRtckReact = 16,
        SelectIf = 17,
        RwMemArm79 = 18,
        GetCounters = 19,
        ReadDcc = 20,
        GetCpuCaps = 21,
        ExecCpuCmd = 22,
        Swo = 23,
        WriteDccEx = 24,
        UpdateFirmwareEx = 25,
        FileIo = 26,
        Register = 27,
        Indicators = 28,
        TestNetSpeed = 29,
        RawTrace = 30,
        // For the legacy capabilities, bit 31 is documented as reserved, but it must be
        // GET_CAPS_EX, since there'd be no other way to know if GET_CAPS_EX is supported.
        GetCapsEx = 31,

        // Extended capabilities

        HwJtagWrite = 32,
        Com = 33,
    }

    flags CapabilityFlags: u128;
);

impl CapabilityFlags {
    fn from_capability(cap: Capability) -> Self {
        Self::from_bits(1 << cap as u32).unwrap()
    }
}

/// A set of capabilities advertised by a probe.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Capabilities(CapabilityFlags);

impl Capabilities {
    /// Creates a `Capabilities` instance from 32 raw bits.
    pub(crate) fn from_raw_legacy(raw: u32) -> Self {
        let mut capabilities = CapabilityFlags::from_bits_truncate(u128::from(raw));
        if capabilities.bits() != u128::from(raw) {
            tracing::debug!(
                "unknown capability bits: 0x{:08X} truncated to 0x{:08X} ({:?})",
                raw,
                capabilities.bits(),
                capabilities,
            );
        }
        // Hide reserved bits from user-facing output.
        capabilities.remove(CapabilityFlags::Reserved0);
        Self(capabilities)
    }

    /// Creates a `Capabilities` instance from a 256-bit bitset.
    pub(crate) fn from_raw_ex(raw: [u8; 32]) -> Self {
        if raw[16..] != [0; 16] {
            tracing::debug!(
                "unknown ext. capability bits: dropping high 16 bytes {:02X?}",
                &raw[16..],
            );
        }
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&raw[..16]);
        let raw = u128::from_le_bytes(bytes);
        let mut capabilities = CapabilityFlags::from_bits_truncate(raw);
        if capabilities.bits() != raw {
            tracing::debug!(
                "unknown ext. capability bits: 0x{:08X} truncated to 0x{:08X} ({:?})",
                raw,
                capabilities.bits(),
                capabilities,
            );
        }
        // Hide reserved bits from user-facing output.
        capabilities.remove(CapabilityFlags::Reserved0);
        Self(capabilities)
    }

    /// Determines whether `self` contains capability `cap`.
    pub fn contains(&self, cap: Capability) -> bool {
        self.0.contains(CapabilityFlags::from_capability(cap))
    }

    /// Determines whether `self` contains all capabilities in `caps`.
    pub fn contains_all(&self, caps: Capabilities) -> bool {
        self.0.contains(caps.0)
    }
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
