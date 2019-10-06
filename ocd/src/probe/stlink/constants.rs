pub mod commands {    
    // Common commands.
    pub const GET_VERSION: u8 = 0xf1;
    pub const JTAG_COMMAND: u8 = 0xf2;
    pub const DFU_COMMAND: u8 = 0xf3;
    pub const SWIM_COMMAND: u8 = 0xf4;
    pub const GET_CURRENT_MODE: u8 = 0xf5;
    pub const GET_TARGET_VOLTAGE: u8 = 0xf7;
    pub const GET_VERSION_EXT: u8 = 0xfb;

    // Modes returned by GET_CURRENT_MODE.
    pub const DEV_DFU_MODE: u8 = 0x00;
    pub const DEV_MASS_MODE: u8 = 0x01;
    pub const DEV_JTAG_MODE: u8 = 0x02;
    pub const DEV_SWIM_MODE: u8 = 0x03;

    // Commands to exit other modes.
    pub const DFU_EXIT: u8 = 0x07;
    pub const SWIM_EXIT: u8 = 0x01;

    // JTAG commands.
    pub const JTAG_READMEM_32BIT: u8 = 0x07;
    pub const JTAG_WRITEMEM_32BIT: u8 = 0x08;
    pub const JTAG_READMEM_8BIT: u8 = 0x0c;
    pub const JTAG_WRITEMEM_8BIT: u8 = 0x0d;
    pub const JTAG_EXIT: u8 = 0x21;
    pub const JTAG_ENTER2: u8 = 0x30;
    pub const JTAG_GETLASTRWSTATUS2: u8 = 0x3e; // From V2J15
    pub const JTAG_DRIVE_NRST: u8 = 0x3c;
    pub const SWV_START_TRACE_RECEPTION: u8 = 0x40;
    pub const SWV_STOP_TRACE_RECEPTION: u8 = 0x41;
    pub const SWV_GET_TRACE_NEW_RECORD_NB: u8 = 0x42;
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
pub enum Status {
    JtagOk = 0x80,
    JtagUnknownError = 0x01,
    JtagSpiError = 0x02,
    JtagDmaError = 0x03,
    JtagUnknownJtagChain = 0x04,
    JtagNoDeviceConnected = 0x05,
    JtagInternalError = 0x06,
    JtagCmdWait = 0x07,
    JtagCmdError = 0x08,
    JtagGetIdcodeError = 0x09,
    JtagAlignmentError = 0x0A,
    JtagDbgPowerError = 0x0B,
    JtagWriteError = 0x0C,
    JtagWriteVerifError = 0x0D,
    JtagAlreadyOpenedInOtherMode = 0x0E,
    SwdApWait = 0x10,
    SwdApFault = 0x11,
    SwdApError = 0x12,
    SwdApParityError = 0x13,
    SwdDpWait = 0x14,
    SwdDpFault = 0x15,
    SwdDpError = 0x16,
    SwdDpParityError = 0x17,
    SwdApWdataError = 0x18,
    SwdApStickyError = 0x19,
    SwdApStickyorunError = 0x1A,
    SwvNotAvailable = 0x20,
    JtagFreqNotSupported = 0x41,
    JtagUnknownCmd = 0x42,
}

/// Map from SWD frequency in Hertz to delay loop count.
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

/// Map from JTAG frequency in Hertz to frequency divider.
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