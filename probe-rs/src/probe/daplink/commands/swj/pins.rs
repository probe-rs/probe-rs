use super::super::{Category, Request, Response, Result};

pub struct SWJPinsRequest {
    /// A mask of the values the different pins selected in the selection mask will be set to.
    pub(crate) output: Pins,
    /// A mask to select all the pins that should be toggled.
    pub(crate) select: Pins,
    /// A timeout to wait for until the pin state is read back.
    pub(crate) wait: u32,
}

bitfield::bitfield! {
    #[derive(Copy, Clone)]
    pub struct Pins(u8);
    impl Debug;
    pub nreset, set_nreset: 7;
    pub ntrst, set_ntrst: 5;
    pub tdo, set_tdo: 3;
    pub tdi, set_tdi: 2;
    pub swdio_tms, set_swdio_tms: 1;
    pub swclk_tck, set_swclk_tck: 0;
}

impl Request for SWJPinsRequest {
    const CATEGORY: Category = Category(0x10);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        use scroll::{Pwrite, LE};

        buffer
            .pwrite_with(self.output.0, offset, LE)
            .expect("This is a bug. Please report it.");
        buffer
            .pwrite_with(self.select.0, offset + 1, LE)
            .expect("This is a bug. Please report it.");
        buffer
            .pwrite_with(self.wait, offset + 2, LE)
            .expect("This is a bug. Please report it.");
        Ok(4)
    }
}

impl Response for Pins {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(Pins(buffer[offset]))
    }
}

pub type SWJPinsResponse = Pins;
