use super::super::{CommandId, Request, SendError};
use crate::architecture::arm::Pins;

pub struct SWJPinsRequest {
    /// A mask of the values the different pins selected in the selection mask will be set to.
    pub(crate) output: Pins,
    /// A mask to select all the pins that should be toggled.
    pub(crate) select: Pins,
    /// A timeout to wait for until the pin state is read back.
    pub(crate) wait: u32,
}

impl SWJPinsRequest {
    pub fn from_raw_values(pin_out: u8, pin_select: u8, pin_wait: u32) -> Self {
        Self {
            output: Pins(pin_out),
            select: Pins(pin_select),
            wait: pin_wait,
        }
    }
}

#[derive(Debug, Default)]
pub struct SWJPinsRequestBuilder {
    nreset: Option<bool>,
    ntrst: Option<bool>,
    tdo: Option<bool>,
    tdi: Option<bool>,
    swdio_tms: Option<bool>,
    swclk_tck: Option<bool>,

    timeout: u32,
}

impl SWJPinsRequestBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn nreset(&mut self, value: bool) -> &mut Self {
        self.nreset = Some(value);
        self
    }

    pub fn ntrst(&mut self, value: bool) -> &mut Self {
        self.ntrst = Some(value);
        self
    }

    pub fn _tdo(&mut self, value: bool) -> &mut Self {
        self.tdo = Some(value);
        self
    }

    pub fn _tdi(&mut self, value: bool) -> &mut Self {
        self.tdi = Some(value);
        self
    }

    pub fn _swdio_tms(&mut self, value: bool) -> &mut Self {
        self.swdio_tms = Some(value);
        self
    }

    pub fn _swclk_tck(&mut self, value: bool) -> &mut Self {
        self.swclk_tck = Some(value);
        self
    }

    pub fn _wait(&mut self, value: u32) -> &mut Self {
        self.timeout = value;
        self
    }

    pub fn build(&self) -> SWJPinsRequest {
        let mut mask = Pins(0);
        let mut values = Pins(0);

        if let Some(nreset) = self.nreset {
            mask.set_nreset(true);
            values.set_nreset(nreset);
        }

        if let Some(ntrst) = self.ntrst {
            mask.set_ntrst(true);
            values.set_ntrst(ntrst);
        }
        if let Some(tdo) = self.tdo {
            mask.set_tdo(true);
            values.set_tdo(tdo);
        }
        if let Some(tdi) = self.tdi {
            mask.set_tdi(true);
            values.set_tdi(tdi);
        }
        if let Some(swdio_tms) = self.swdio_tms {
            mask.set_swdio_tms(true);
            values.set_swdio_tms(swdio_tms);
        }
        if let Some(swclk_tck) = self.swclk_tck {
            mask.set_swclk_tck(true);
            values.set_swclk_tck(swclk_tck);
        }

        SWJPinsRequest {
            output: values,
            select: mask,
            wait: self.timeout,
        }
    }
}

impl Request for SWJPinsRequest {
    const COMMAND_ID: CommandId = CommandId::SwjPins;

    type Response = SWJPinsResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        use scroll::{LE, Pwrite};

        buffer
            .pwrite_with(self.output.0, 0, LE)
            .expect("Buffer for CMSIS-DAP command is too small. This is a bug, please report it.");
        buffer
            .pwrite_with(self.select.0, 1, LE)
            .expect("Buffer for CMSIS-DAP command is too small. This is a bug, please report it.");
        buffer
            .pwrite_with(self.wait, 2, LE)
            .expect("Buffer for CMSIS-DAP command is too small. This is a bug, please report it.");
        Ok(6)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        Ok(Pins(buffer[0]))
    }
}

pub type SWJPinsResponse = Pins;
