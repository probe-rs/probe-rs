use enum_primitive_derive::Primitive;
use num_traits::FromPrimitive;
use scroll::{Pread, LE};

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Commands {
    Query = 0x00,
    Set = 0x01,
    Get = 0x02,
    StartSession = 0x10,
    EndSession = 0x11,
    JtagDetect = 0x30,
    JtagCalOsc = 0x31,
    JtagFwUpgrade = 0x50,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum Responses {
    StatusOk = 0x80,       //  All OK
    List = 0x81,           //  List of items returned
    Data = 0x84,           //  Data returned
    Failed = 0xA0,         // Command failed to execute
    FailedWithData = 0xA1, // Command failed to execute with data returned
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum Events {
    Power = 0x10,
    Sleep = 0x11,
    ExtReset = 0x12,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum FailureCodes {
    Ok = 0x00,
    NotSupported = 0x10,
    InvalidKey = 0x11,
    InvalidParameter = 0x12,
    InvalidParameterValue = 0x13,
    JtagDetectNoDevices = 0x30,
    JtagDetectTooManyDevices = 0x31,
    JtagDetectJtagmInitError = 0x32,
    JtagDetectJtagmError = 0x33,
    NoTargetPower = 0x38,
    OsccalInvalidMode = 0x40,
    OsccalInvalidPhysical = 0x41,
    OsccalFwError = 0x42,
    OsccalFailed = 0x43,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum SetGetFailureCodes {
    Ok = 0x00,
    NotImplemented = 0x10,
    NotSupported = 0x11,
    InvalidClockSpeed = 0x20,
    IllegalState = 0x21,
    JtagmInitError = 0x22,
    InvalidValue = 0x23,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum QueryContexts {
    QueryCommands = 0x00,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum SetGetContexts {
    Config = 0x00,
    Analaog = 0x01,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum ConfigContextPrameters {
    HwRev = 0x00,
    FwRevMaj = 0x01,
    HwRevMin = 0x02,
    Build = 0x03,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum AnalogContextParameters {
    VtRef = 0x00,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum PowerEvents {
    On = 0x00,
    Off = 0x01,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum SleepEvents {
    Awake = 0x00,
    Sleep = 0x01,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
pub enum ResetEvents {
    Released = 0x00,
    Applied = 0x01,
}

/// Parsed responses
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Ok,
    List(Vec<u8>),
    Data(Vec<u8>),
    Pc(u32),
    Failed(FailureCodes),
}

impl Response {
    pub fn parse_response(response: &[u8]) -> Self {
        match Responses::from_u8(response[0]).expect("Response does not contain valid response id")
        {
            Responses::StatusOk => Response::Ok,
            Responses::List => Response::List(response[2..].to_vec()),
            Responses::Data => {
                if *response.last().expect("No status in response") == 0x00 {
                    Response::Data(response[2..response.len() - 1].to_vec())
                } else {
                    panic!("Invalid data returned in housekeeping response");
                }
            }
            Responses::Failed => Response::Failed(
                FailureCodes::from_u8(response[2]).expect("Unable to find matching error code"),
            ),
            Responses::FailedWithData => panic!("FailedWithData should newer be returned"),
        }
    }
}
