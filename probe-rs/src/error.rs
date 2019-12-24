//! Error handling
//!
//!
use crate::config::memory::FlashRegion;
use std::error;
use std::fmt;

#[macro_export]
macro_rules! u {
    ($kind:expr, $source:expr) => {{
        match $source {
            Ok(v) => v,
            Err(e) => return Err(Error::new_with_source($kind, Some(e))),
        }
    }};
}

#[macro_export]
macro_rules! dp {
    ($source:expr) => {
        u!(DebugProbe, $source)
    };
}

#[macro_export]
macro_rules! rt {
    ($source:expr) => {
        u!(Romtable, $source)
    };
}

#[macro_export]
macro_rules! res {
    ($kind:expr, $source:expr) => {{
        Err(Error::new_with_source(
            ErrorKind::from($kind),
            Some($source),
        ))
    }};
    ($kind:expr) => {{
        Err(Error::new(ErrorKind::from($kind)))
    }};
}

#[macro_export]
macro_rules! err {
    ($kind:expr, $source:expr) => {{
        Error::new_with_source(ErrorKind::from($kind), Some($source))
    }};
    ($kind:expr) => {{
        Error::new(ErrorKind::from($kind))
    }};
}

pub use crate::{dp, err, res, rt, u};
pub use ErrorKind::*;

#[derive(Debug)]
pub enum ErrorKind {
    /// Error getting target information from the registry
    Registry,
    // DebugProbeError(DebugProbeError),
    // RomTableError(RomTableError),
    DebugProbe,
    Usb,
    // VoltageDivisionByZero,
    // UnknownMode,
    // JTagDoesNotSupportMultipleAP,
    // TransferFault(u32, u16),
    // DataAlignmentError,
    // Access16BitNotSupported,
    // BlanksNotAllowedOnDPRegister,
    // RegisterAddressMustBe16Bit,
    NotEnoughBytesRead,
    // EndpointNotFound,
    ProbeCouldNotBeCreated,
    // TargetPowerUpFailed,
    UnexpectedDapAnswer,
    DapCommunicationFailure,
    TargetPowerUpFailed,

    Timeout,
    UnknownError,
    NotFound(NotFoundKind),
    Missing(MissingKind),
    Io,
    Yaml,
    IHexRead,

    AccessPort,
    InvalidAccessPortNumber,
    MemoryNotAligned,
    RegisterRead {
        addr: u8,
        name: &'static str,
    },
    RegisterWrite {
        addr: u8,
        name: &'static str,
    },
    OutOfBounds,

    Romtable,
    NotARomtable,
    CSComponentIdentification,

    Flasher,
    // Init(u32),
    // Uninit(u32),
    // EraseAll(u32),
    EraseAllNotSupported,
    CallFailed {
        call: String,
        result: u32,
    },
    // EraseSector(u32, u32),
    // ProgramPage(u32, u32),
    // UnalignedFlashWriteAddress,
    // UnalignedPhraseLength,
    // ProgramPhrase(u32, u32),
    // AnalyzerNotSupported,
    // SizeNotPowerOf2,
    // AddressNotMultipleOfSize,
    AddressNotInRegion {
        address: u32,
        region: FlashRegion,
    },

    FlashBuilder,
    // AddressBeforeFlashStart(u32),   // Contains faulty address.
    // DataOverlap(u32),               // Contains faulty address.
    // InvalidFlashAddress(u32),       // Contains faulty address.
    // DuplicateDataEntry(u32),        // There is two entries for data at the same address.
    // PageSizeDoesNotMatch(u32, u32), // The flash sector size is not a multiple of the flash page size.
    // MaxPageCountExceeded(usize),
    // ProgramPage(u32, u32),
    // Flasher(FlasherError),
    FlashLoader,
    NoSuitableFlash(u32),

    FileDownloader,

    Custom(String),
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Option<Box<dyn error::Error + Send + Sync + 'static>>,
}

impl Error {
    pub fn new_with_source(
        kind: ErrorKind,
        source: Option<impl error::Error + Send + Sync + 'static>,
    ) -> Self {
        Self {
            kind,
            source: source.map(|s| Box::new(s) as _),
        }
    }

    pub fn new(kind: ErrorKind) -> Self {
        Self { kind, source: None }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source.as_ref().map(|x| &**x as _)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use ErrorKind::*;

        match &self.kind {
            Registry => write!(f, "An error within the registry was encountered"),
            NotFound(nfk) => write!(f, "{} was not found", nfk),
            Missing(mk) => write!(f, "{} is missing", mk),
            Io => write!(f, "An IO error was encountered"),
            Yaml => write!(f, "The yaml parser failed"),
            IHexRead => write!(f, "The ihex reader encountered an error"),

            Usb => write!(f, "An error with USB was encountered"),
            ProbeCouldNotBeCreated => write!(f, "Probe object could not be created"),
            NotEnoughBytesRead => write!(f, "Not enough bytes were read from the USB endpoint"),

            AccessPort => write!(
                f,
                "An error during operation with the access point was encountered"
            ),
            InvalidAccessPortNumber => write!(f, "Invalid access port number"),
            MemoryNotAligned => write!(f, "Misaligned memory access"),
            RegisterRead { addr, name } => write!(
                f,
                "Failed to read register {}, address 0x{:08x}",
                name, addr
            ),
            RegisterWrite { addr, name } => write!(
                f,
                "Failed to write register {}, address 0x{:08x}",
                name, addr
            ),
            OutOfBounds => write!(f, "Out of bounds access"),

            NotARomtable => write!(f, "Component is not a valid rom table"),
            CSComponentIdentification => write!(f, "Failed to identify CoreSight component"),

            Flasher => write!(f, "The flasher encountered an error"),
            CallFailed { call, result } => {
                write!(f, "The call to {} failed with a result of {}", call, result)
            }
            EraseAllNotSupported => {
                write!(f, "Erase all is not supported with this flash algorithm")
            }
            AddressNotInRegion { address, region } => write!(
                f,
                "The region 0x{:x}..0x{:x} does not contain the address 0x{:x}",
                region.range.start, region.range.end, address
            ),

            FlashBuilder => write!(f, "The flash builder encountered an error"),

            NoSuitableFlash(addr) => {
                write!(f, "No flash memory was found at address {:#08x}.", addr)
            }
            FlashLoader => write!(f, "The flash loader encountered an error"),

            FileDownloader => write!(f, "The file downloader encountered an error"),
            _ => unimplemented!(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum NotFoundKind {
    Chip,
    Algorithm,
    Core,
    CtrlAp,
    ChipInfo,
    Endpoint,
}

impl fmt::Display for NotFoundKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use NotFoundKind::*;

        match self {
            Chip => write!(f, "chip"),
            Algorithm => write!(f, "algorithm"),
            Core => write!(f, "core"),
            CtrlAp => write!(f, "control access port"),
            ChipInfo => write!(f, "chip info"),
            Endpoint => write!(f, "endpoint"),
        }
    }
}

#[derive(Debug)]
pub enum MissingKind {
    RamRegion,
    FlashRegion,
}

impl fmt::Display for MissingKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use MissingKind::*;

        match self {
            RamRegion => write!(f, "ram region"),
            FlashRegion => write!(f, "flash region"),
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(value: ErrorKind) -> Error {
        Error::new(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Error {
        Error::new_with_source(ErrorKind::Io, Some(Box::new(value)))
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(value: serde_yaml::Error) -> Error {
        Error::new_with_source(ErrorKind::Yaml, Some(value))
    }
}

impl From<ihex::reader::ReaderError> for Error {
    fn from(value: ihex::reader::ReaderError) -> Error {
        Error::new_with_source(ErrorKind::IHexRead, Some(value))
    }
}

impl From<rusb::Error> for Error {
    fn from(value: rusb::Error) -> Error {
        Error::new_with_source(ErrorKind::Usb, Some(value))
    }
}

impl From<hidapi::HidError> for Error {
    fn from(_value: hidapi::HidError) -> Error {
        // std::error::Error is not implemented for hidapo::HidError,
        // which is why we cannot propagate it here.
        Error::new(ErrorKind::Usb)
    }
}

impl From<(ErrorKind, Error)> for Error {
    fn from(value: (ErrorKind, Error)) -> Error {
        Error::new_with_source(value.0, Some(value.1))
    }
}
