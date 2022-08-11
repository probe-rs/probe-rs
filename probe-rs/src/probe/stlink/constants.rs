#![allow(unused)]

use enum_primitive_derive::Primitive;

pub mod commands {
    // Common commands.
    pub const GET_VERSION: u8 = 0xf1;
    pub const JTAG_COMMAND: u8 = 0xf2;
    pub const DFU_COMMAND: u8 = 0xf3;
    pub const SWIM_COMMAND: u8 = 0xf4;
    pub const GET_CURRENT_MODE: u8 = 0xf5;
    pub const GET_TARGET_VOLTAGE: u8 = 0xf7;
    pub const GET_VERSION_EXT: u8 = 0xfb;

    // Commands to exit other modes.
    pub const DFU_EXIT: u8 = 0x07;
    pub const SWIM_EXIT: u8 = 0x01;

    // JTAG commands.
    pub const JTAG_READMEM_32BIT: u8 = 0x07;
    pub const JTAG_WRITEMEM_32BIT: u8 = 0x08;
    pub const JTAG_READMEM_8BIT: u8 = 0x0c;
    pub const JTAG_WRITEMEM_8BIT: u8 = 0x0d;
    pub const JTAG_EXIT: u8 = 0x21;
    pub const JTAG_ENTER: u8 = 0x20;

    // The following commands are from Version 2 of the API,
    // supported
    pub const JTAG_ENTER2: u8 = 0x30;

    pub const JTAG_READ_CORE_REG: u8 = 0x33;
    pub const JTAG_WRITE_CORE_REG: u8 = 0x34;

    pub const JTAG_WRITE_DEBUG_REG: u8 = 0x35;
    pub const JTAG_READ_DEBUG_REG: u8 = 0x36;

    pub const JTAG_GETLASTRWSTATUS2: u8 = 0x3e; // From V2J15
    pub const JTAG_DRIVE_NRST: u8 = 0x3c;
    pub const SWO_START_TRACE_RECEPTION: u8 = 0x40;
    pub const SWO_STOP_TRACE_RECEPTION: u8 = 0x41;
    pub const SWO_GET_TRACE_NEW_RECORD_NB: u8 = 0x42;
    pub const SWD_SET_FREQ: u8 = 0x43; // From V2J20
    pub const JTAG_SET_FREQ: u8 = 0x44; // From V2J24
    pub const JTAG_READ_DAP_REG: u8 = 0x45; // From V2J24
    pub const JTAG_WRITE_DAP_REG: u8 = 0x46; // From V2J24
    pub const JTAG_READMEM_16BIT: u8 = 0x47; // From V2J26
    pub const JTAG_WRITEMEM_16BIT: u8 = 0x48; // From V2J26
    pub const JTAG_INIT_AP: u8 = 0x4b; // From V2J28
    pub const JTAG_CLOSE_AP_DBG: u8 = 0x4c; // From V2J28
    pub const SET_COM_FREQ: u8 = 0x61; // V3 only, replaces SWD/JTAG_SET_FREQ
    pub const GET_COM_FREQ: u8 = 0x62; // V3 only

    // Parameters for JTAG_ENTER2.
    pub const JTAG_ENTER_SWD: u8 = 0xa3;
    pub const JTAG_ENTER_JTAG_NO_CORE_RESET: u8 = 0xa3;

    // Parameters for JTAG_DRIVE_NRST.
    pub const JTAG_DRIVE_NRST_LOW: u8 = 0x00;
    pub const JTAG_DRIVE_NRST_HIGH: u8 = 0x01;
    pub const JTAG_DRIVE_NRST_PULSE: u8 = 0x02;

    // Parameters for JTAG_INIT_AP and JTAG_CLOSE_AP_DBG.
    pub const JTAG_AP_NO_CORE: u8 = 0x00;
    pub const JTAG_AP_CORTEXM_CORE: u8 = 0x01;

    // Parameters for SET_COM_FREQ and GET_COM_FREQ.
    pub const JTAG_STLINK_SWD_COM: u8 = 0x00;
    pub const JTAG_STLINK_JTAG_COM: u8 = 0x01;
}

/// STLink status codes and messages.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    JtagOk,
    JtagUnknownError,
    JtagSpiError,
    JtagDmaError,
    JtagUnknownJtagChain,
    JtagNoDeviceConnected,
    JtagInternalError,
    JtagCmdWait,
    JtagCmdError,
    JtagGetIdcodeError,
    JtagAlignmentError,
    JtagDbgPowerError,
    JtagWriteError,
    JtagWriteVerifError,
    JtagAlreadyOpenedInOtherMode,
    SwdApWait,
    SwdApFault,
    SwdApError,
    SwdApParityError,
    SwdDpWait,
    SwdDpFault,
    SwdDpError,
    SwdDpParityError,
    SwdApWdataError,
    SwdApStickyError,
    SwdApStickyorunError,
    SwoNotAvailable,
    JtagFreqNotSupported,
    JtagUnknownCmd,
    Other(u8),
}

impl From<u8> for Status {
    fn from(value: u8) -> Status {
        use Status::*;
        match value {
            0x80 => JtagOk,
            0x01 => JtagUnknownError,
            0x02 => JtagSpiError,
            0x03 => JtagDmaError,
            0x04 => JtagUnknownJtagChain,
            0x05 => JtagNoDeviceConnected,
            0x06 => JtagInternalError,
            0x07 => JtagCmdWait,
            0x08 => JtagCmdError,
            0x09 => JtagGetIdcodeError,
            0x0A => JtagAlignmentError,
            0x0B => JtagDbgPowerError,
            0x0C => JtagWriteError,
            0x0D => JtagWriteVerifError,
            0x0E => JtagAlreadyOpenedInOtherMode,
            0x10 => SwdApWait,
            0x11 => SwdApFault,
            0x12 => SwdApError,
            0x13 => SwdApParityError,
            0x14 => SwdDpWait,
            0x15 => SwdDpFault,
            0x16 => SwdDpError,
            0x17 => SwdDpParityError,
            0x18 => SwdApWdataError,
            0x19 => SwdApStickyError,
            0x1A => SwdApStickyorunError,
            0x20 => SwoNotAvailable,
            0x41 => JtagFreqNotSupported,
            0x42 => JtagUnknownCmd,
            v => Other(v),
        }
    }
}

/// Map from SWD frequency in Hertz to delay loop count.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SwdFrequencyToDelayCount {
    Hz4600000 = 0,
    Hz1800000 = 1, // Default
    Hz1200000 = 2,
    Hz950000 = 3,
    Hz650000 = 5,
    Hz480000 = 7,
    Hz400000 = 9,
    Hz360000 = 10,
    Hz240000 = 15,
    Hz150000 = 25,
    Hz125000 = 31,
    Hz100000 = 40,
}

impl SwdFrequencyToDelayCount {
    /// Try to find an appropriate setting for the given frequency in kHz.
    ///
    /// If a direct match is not found, return the setting for a lower frequency
    /// if possible. If this is not possible, returns `None`.
    pub(crate) fn find_setting(frequency: u32) -> Option<SwdFrequencyToDelayCount> {
        use SwdFrequencyToDelayCount::*;

        Some(match frequency {
            _ if frequency >= 4_600 => Hz4600000,
            _ if frequency >= 1_800 => Hz1800000,
            _ if frequency >= 1_200 => Hz1200000,
            _ if frequency >= 950 => Hz950000,
            _ if frequency >= 650 => Hz650000,
            _ if frequency >= 480 => Hz480000,
            _ if frequency >= 400 => Hz400000,
            _ if frequency >= 360 => Hz360000,
            _ if frequency >= 240 => Hz240000,
            _ if frequency >= 150 => Hz150000,
            _ if frequency >= 125 => Hz125000,
            _ if frequency >= 100 => Hz100000,
            _ => {
                return None;
            }
        })
    }

    /// Get the SWD frequency in kHz
    pub(crate) fn to_khz(self) -> u32 {
        use SwdFrequencyToDelayCount::*;

        match self {
            Hz4600000 => 4600,
            Hz1800000 => 1800,
            Hz1200000 => 1200,
            Hz950000 => 950,
            Hz650000 => 650,
            Hz480000 => 480,
            Hz400000 => 400,
            Hz360000 => 360,
            Hz240000 => 240,
            Hz150000 => 150,
            Hz125000 => 125,
            Hz100000 => 100,
        }
    }
}

/// Map from JTAG frequency in Hertz to frequency divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JTagFrequencyToDivider {
    Hz18000000 = 2,
    Hz9000000 = 4,
    Hz4500000 = 8,
    Hz2250000 = 16,
    Hz1120000 = 32, // Default
    Hz560000 = 64,
    Hz280000 = 128,
    Hz140000 = 256,
}

impl JTagFrequencyToDivider {
    /// Try to find an appropriate setting for the given frequency in kHz.
    ///
    /// If a direct match is not found, return the setting for a higher frequency
    /// if possible. If this is not possible, returns `None`.
    pub(crate) fn find_setting(frequency: u32) -> Option<Self> {
        use JTagFrequencyToDivider::*;

        Some(match frequency {
            _ if frequency >= 18_000 => Hz18000000,
            _ if frequency >= 9_000 => Hz9000000,
            _ if frequency >= 4_500 => Hz4500000,
            _ if frequency >= 2_225 => Hz2250000,
            _ if frequency >= 1_120 => Hz1120000,
            _ if frequency >= 560 => Hz560000,
            _ if frequency >= 280 => Hz280000,
            _ if frequency >= 140 => Hz140000,
            _ => {
                return None;
            }
        })
    }

    /// Return the frequency in kHz
    pub(crate) fn to_khz(self) -> u32 {
        use JTagFrequencyToDivider::*;

        match self {
            Hz18000000 => 18_000,
            Hz9000000 => 9_000,
            Hz4500000 => 4_500,
            Hz2250000 => 2_250,
            Hz1120000 => 1_120,
            Hz560000 => 560,
            Hz280000 => 280,
            Hz140000 => 140,
        }
    }
}

/// Modes returned by GET_CURRENT_MODE.
#[derive(Debug)]
pub(crate) enum Mode {
    /// Device is in DFU (Device Firmware Update) mode
    Dfu = 0x00,
    /// Device is in mass storage mode?
    MassStorage = 0x01,
    /// Device is in JTAG mode
    Jtag = 0x02,
    /// Device is in SWIM (Single Wire Interface) mode
    Swim = 0x03,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_too_low_swd_speed() {
        assert!(SwdFrequencyToDelayCount::find_setting(0).is_none());
        assert!(SwdFrequencyToDelayCount::find_setting(1).is_none());
        assert!(SwdFrequencyToDelayCount::find_setting(99).is_none());
    }

    #[test]
    fn test_swd_speed() {
        assert_eq!(
            SwdFrequencyToDelayCount::find_setting(100).unwrap(),
            SwdFrequencyToDelayCount::Hz100000
        );
        assert_eq!(
            SwdFrequencyToDelayCount::find_setting(124).unwrap(),
            SwdFrequencyToDelayCount::Hz100000
        );
        assert_eq!(
            SwdFrequencyToDelayCount::find_setting(125).unwrap(),
            SwdFrequencyToDelayCount::Hz125000
        );

        assert_eq!(
            SwdFrequencyToDelayCount::find_setting(46_000).unwrap(),
            SwdFrequencyToDelayCount::Hz4600000
        );
        assert_eq!(
            SwdFrequencyToDelayCount::find_setting(u32::max_value()).unwrap(),
            SwdFrequencyToDelayCount::Hz4600000
        );
    }

    #[test]
    fn test_too_low_jtag_speed() {
        assert!(JTagFrequencyToDivider::find_setting(0).is_none());
        assert!(JTagFrequencyToDivider::find_setting(1).is_none());
        assert!(JTagFrequencyToDivider::find_setting(139).is_none());
    }

    #[test]
    fn test_jtag_speed() {
        assert_eq!(
            JTagFrequencyToDivider::find_setting(140).unwrap(),
            JTagFrequencyToDivider::Hz140000
        );
        assert_eq!(
            JTagFrequencyToDivider::find_setting(279).unwrap(),
            JTagFrequencyToDivider::Hz140000
        );
        assert_eq!(
            JTagFrequencyToDivider::find_setting(280).unwrap(),
            JTagFrequencyToDivider::Hz280000
        );

        assert_eq!(
            JTagFrequencyToDivider::find_setting(18_000).unwrap(),
            JTagFrequencyToDivider::Hz18000000
        );
        assert_eq!(
            JTagFrequencyToDivider::find_setting(u32::max_value()).unwrap(),
            JTagFrequencyToDivider::Hz18000000
        );
    }
}
