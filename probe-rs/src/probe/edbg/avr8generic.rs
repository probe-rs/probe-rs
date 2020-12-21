use crate::probe::edbg::EDBGprobe;
pub struct Avr8GenericProtocol {}

impl Avr8GenericProtocol {
    fn new(probe: &EDBGprobe) -> Self {
        Avr8GenericProtocol {}
    }
}
enum Avr8GenericCommands {
    Query = 0x00,              // Capability discovery
    Set = 0x01,                // Set parameters
    Get = 0x02,                // Get parameters
    ActivatePhysical = 0x10,   // Connect physically
    DeactivatePhysical = 0x11, // Disconnect physically
    GetId = 0x12,              // Read the ID
    Attach = 0x13,             // Attach to OCD module
    Detach = 0x14,             // Detach from OCD module
    ProgModeEnter = 0x15,      // Enter programming mode
    ProgModeLeave = 0x16,      // Leave programming mode
    DisableDebugwire = 0x17,   // Disable debugWIRE interface
    Erase = 0x20,              // Erase the chip
    MemoryRead = 0x21,         // Read memory
    MemoryReadMasked = 0x22,   // Read memory while via a mask
    MemoryWrite = 0x23,        // Write memory
    Crc = 0x24,                // Calculate CRC
    Reset = 0x30,              // Reset the MCU
    Stop = 0x31,               // Stop the MCU
    Run = 0x32,                // Resume execution
    RunToAddress = 0x33,       // Resume with breakpoint
    Step = 0x34,               // Single step
    PcRead = 0x35,             // Read PC
    PcWrite = 0x36,            // Write PC
    HwBreakSet = 0x40,         // Set breakpoints
    HwBreakClear = 0x41,       // Clear breakpoints
    SwBreakSet = 0x43,         // Set software breakpoints
    SwBreakClear = 0x44,       // Clear software breakpoints
    SwBreakClearAll = 0x45,    // Clear all software breakpoints
    PageErase = 0x50,          // Erase page
}

enum Avr8GenericResponses {
    StatusOk = 0x80, //  All OK
    List = 0x81,     //  List of items returned
    Data = 0x84,     //  Data returned
    Pc = 0x83,       //  PC value returned
    Failed = 0xA0,   // Command failed to execute
}

// Protocol events
enum Avr8GenericEvents {
    Break = 0x40,
    Idr = 0x41,
}

// Failure response codes (RSP_FAILED)
enum Avr8GenericFailureCodes {
    StatusOk = 0x00,             // All OK
    DwPhyError = 0x10,           // debugWIRE physical error
    JtagmInitError = 0x11,       // JTAGM failed to initialise
    JtagmError = 0x12,           // JTAGM did something strange
    JtagError = 0x13,            // JTAG low level error
    JtagmVersion = 0x14,         // Unsupported version of JTAGM
    JtagmTimeout = 0x15,         // JTAG master timed out
    JtagBitBangerTimeout = 0x16, // JTAG bit banger timed out
    ParityError = 0x17,          // Parity error in received data
    EbError = 0x18,              // Did not receive EMPTY byte
    PdiTimeout = 0x19,           // PDI physical timed out
    Collision = 0x1A,            // Collision on physical level
    PdiEnable = 0x1B,            // PDI enable failed
    NoDeviceFound = 0x20,        // devices == 0!
    ClockError = 0x21,           // Failure when increasing baud
    NoTargetPower = 0x22,        // Target power not detected
    NotAttached = 0x23,          // Must run attach command first
    DaisyChainTooLong = 0x24,    // Devices > 31
    DaisyChainConfig = 0x25,     // Configured device bits do not add up to
    InvalidPhysicalState = 0x31, // Physical not activated
    IllegalState = 0x32,         // Illegal run / stopped state
    InvalidConfig = 0x33,        // Invalid config for activate phy
    InvalidMemtype = 0x34,       // Not a valid memtype
    InvalidSize = 0x35,          // Too many or too few bytes
    InvalidAddress = 0x36,       // Asked for a bad address
    InvalidAlignment = 0x37,     // Asked for badly aligned data
    IllegalMemoryRange = 0x38,   // Address not within legal range
    IllegalValue = 0x39,         // Illegal value given
    IllegalId = 0x3A,            // Illegal target ID
    InvalidClockSpeed = 0x3B,    // Clock value out of range
    Timeout = 0x3C,              // A timeout occurred
    IllegalOcdStatus = 0x3D,     // Read an illegal OCD status
    NvmEnable = 0x40,            // NVM failed to be enabled
    NvmDisable = 0x41,           // NVM failed to be disabled
    CsError = 0x42,              // Illegal control/status bits
    CrcFailure = 0x43,           // CRC mismatch
    OcdLocked = 0x44,            // Failed to enable OCD
    NoOcdControl = 0x50,         // Device is not under control
    PcReadFailed = 0x60,         // Error when reading PC
    RegisterReadFailed = 0x61,   // Error when reading register
    ReadError = 0x70,            // Error while reading
    WriteError = 0x71,           // Error while writing
    WriteTimeout = 0x72,         // Timeout while reading
    IllegalBreakpoint = 0x80,    // Invalid breakpoint configuration
    TooManyBreakpoints = 0x81,   // Not enough available resources
    NotSupported = 0x90,         // This feature is not available
    NotImplemented = 0x91,       // Command has not been implemented
    Unknown = 0xFF,              //Disaster.
}

// QUERY types on this protocol
enum Avr8GenericQueryContexts {
    Commands = 0x00,      // Supported command list
    Configuration = 0x05, // Supported configuration list
    ReadMemtypes = 0x07,  // Supported read memtypes list
    WriteMemtypes = 0x08, // Supported write memtypes list
}

#[derive(Clone, Copy, Debug, PartialEq, Primitive)]
enum Avr8GenericSetGetContexts {
    Config = 0x00,
    Physical = 0x01,
    Device = 0x02,
    Options = 0x03,
    Session = 0x04,
}

enum Avr8GenericConfigContextParameters {
    Variant = 0x00,  // Device family/variant
    Function = 0x01, // Functional intent
}

enum Avr8GenericPhysicalContextParameters {
    Interface = 0x00,  // Physical interface selector
    JtagDaisY = 0x01,  // JTAG daisy chain settings
    DwClkDiv = 0x10,   // debugWIRE clock divide ratio
    MegaPrgClk = 0x20, // Clock for programming megaAVR
    MegaDbgClk = 0x21, // Clock for debugging megaAVR
    XmJtagClk = 0x30,  // JTAG clock for AVR XMEGA
    XmPdiClK = 0x31,   // PDI clock for AVR XMEGA and AVR devices with UPDI
}

enum Avr8GenericOptionsContextParameters {
    RunTimers = 0x00,    //  Keep timers running when stopped
    DisableDrp = 0x01,   //  No data breaks during reset
    EnableIdr = 0x03,    //  Relay IDR messages
    PollInterval = 0x04, //  Configure polling interval
}

enum Avr8GenericSessionContextParameters {
    AVR8_SESS_MAIN_PC = 0x00, // Address of main() function (deprecated)
}
enum Avr8GenericConfigTestParameters {
    TargetRunning = 0x00, // Is target running?
}

enum Avr8GenericVariantValues {
    Loopback = 0x00, //  Dummy device
    Dw = 0x01,       //  tinyAVR or megaAVR with debugWIRE
    Megajtag = 0x02, //  megaAVR with JTAG
    Xmega = 0x03,    //  AVR XMEGA
    Updi = 0x05,     //  AVR devices with UPDI
    None = 0xFF,     //  No device
}

enum Avr8GenericFunctionValues {
    None = 0x00,        // Not configured
    Programming = 0x01, // I want to program only
    Debugging = 0x02,   // I want a debug session
}

// Physical modes
enum Avr8GenericPhysicalInterfaces {
    None = 0x00, //  Not configured
    JTAG = 0x04, //  JTAG
    DW = 0x05,   //  debugWIRE
    PDI = 0x06,  //  PDI
    UPDI = 0x08, //  UPDI (one-wire)
}

enum Avr8GenericMegaBreakpointTypes {
    AVR8_HWBP_PROG_BP = 0x01, // Program breaks
}

enum Avr8GenericMegaBreakCauses {
    Unknown = 0x00, // Unspecified
    Program = 0x01, // Program break
}

enum Avr8GenericXtendedEraseModes {
    Chip = 0x00,       // Erase entire chip
    App = 0x01,        // Erase application section only
    Boot = 0x02,       // Erase boot section only
    Eeprom = 0x03,     // Erase EEPROM section only
    AppPage = 0x04,    // Erase a single app section page
    BootPage = 0x05,   // Erase a single boot section page
    EepromPage = 0x06, // Erase a single EEPROM page
    Usersig = 0x07,    // Erase the user signature section
}

// Memory types
enum Avr8GenericMemtypes {
    SRAM = 0x20,                 //  SRAM
    Eeprom = 0x22,               //  EEPROM memory
    Spm = 0xA0,                  //  Flash memory in a debug session
    FlashPage = 0xB0,            //  Flash memory programming
    EepromPage = 0xB1,           //  EEPROM memory pages
    Fuses = 0xB2,                //  Fuse memory
    Lockbits = 0xB3,             //  Lock bits
    Signature = 0xB4,            // Device signature
    Osccal = 0xB5,               //  Oscillator calibration values
    Regfile = 0xB8,              //  Register file
    ApplFlash = 0xC0,            //  Application section flash
    BootFlash = 0xC1,            //  Boot section flash
    ApplFlashAtomic = 0xC2,      //  Application page with auto-erase
    BootFlashAtomic = 0xC3,      //  Boot page with auto-erase
    EepromAtomic = 0xC4,         //  EEPROM page with auto-erase
    UserSignature = 0xC5,        //  User signature secion
    CalibrationSignature = 0xC6, //  Calibration section
}
