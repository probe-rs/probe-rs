//! Module for interacting with the embedded Trace Memory Controller (TMC)
//!
//! The embedded trace memory controller exposes a means of buffering and storing trace data in
//! on-device SRAM for extraction either via the TPIU or via the debug interface.
use core::iter::Iterator;

use crate::{
    architecture::arm::{
        component::DebugRegister, memory::CoresightComponent, ArmError, ArmProbeInterface,
    },
    Error,
};

use bitfield::bitfield;

const REGISTER_OFFSET_RSZ: u32 = 0x04;
const REGISTER_OFFSET_RRD: u32 = 0x10;
const REGISTER_OFFSET_CTL: u32 = 0x20;
const REGISTER_OFFSET_CBUFLVL: u32 = 0x30;

#[repr(u8)]
pub enum Mode {
    /// Trace memory is used as a circular buffer. When the buffer fills, incoming trace data will
    /// overwrite older trace memory until the trace is stopped.
    Circular = 0b00,

    /// The trace memory is used as a FIFO that can be manually read through the RRD register. When
    /// the buffer fills, the incoming trace stream is stalled.
    Software = 0b01,

    /// Trace memory is used as a FIFO that is drained through hardware to the TPIU. Trace data
    /// is captured until the buffer fills, at which point the incoming trace stream is stalled.
    /// Whenever the buffer is non-empty, trace data is drained to the TPIU.
    Hardware = 0b10,
}

/// The embedded trace memory controller.
pub struct TraceMemoryController<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> TraceMemoryController<'a> {
    /// Construct a new embedded trace fifo controller.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Self {
            component,
            interface,
        }
    }

    /// Configure the FIFO operational mode.
    ///
    /// # Args
    /// * `mode` - The desired operational mode of the FIFO.
    pub fn set_mode(&mut self, mode: Mode) -> Result<(), Error> {
        let mut mode_reg = EtfMode::load(self.component, self.interface)?;
        mode_reg.set_mode(mode as _);
        mode_reg.store(self.component, self.interface)?;
        Ok(())
    }

    /// Enable trace captures using the FIFO.
    pub fn enable_capture(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_CTL, 1)?;
        Ok(())
    }

    /// Disable trace captures using the FIFO.
    pub fn disable_capture(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_CTL, 0)?;
        Ok(())
    }

    /// Attempt to read a value out of the FIFO
    pub fn read(&mut self) -> Result<Option<u32>, ArmError> {
        // Read the RRD register.
        match self
            .component
            .read_reg(self.interface, REGISTER_OFFSET_RRD)?
        {
            // The register has a sentinel value to indicate no more data is available in the FIFO.
            0xFFFF_FFFF => Ok(None),

            value => Ok(Some(value)),
        }
    }

    /// Check if the FIFO is full.
    pub fn full(&mut self) -> Result<bool, Error> {
        let status = Status::load(self.component, self.interface)?;
        Ok(status.full())
    }

    /// Check if the FIFO is empty.
    pub fn empty(&mut self) -> Result<bool, Error> {
        let status = Status::load(self.component, self.interface)?;
        Ok(status.empty())
    }

    /// Check if the ET capture has stopped and all internal pipelines and buffers have been
    /// drained.
    pub fn ready(&mut self) -> Result<bool, Error> {
        let status = Status::load(self.component, self.interface)?;
        Ok(status.ready())
    }

    /// Check if the ETF has triggered.
    ///
    /// # Note
    /// This will only be set when operating in circular buffer modes.
    pub fn triggered(&mut self) -> Result<bool, Error> {
        let status = Status::load(self.component, self.interface)?;
        Ok(status.trigd())
    }

    /// Get the current number of bytes within the FIFO.
    ///
    /// # Note
    /// This will always return zero if the capture is disabled.
    pub fn fill_level(&mut self) -> Result<u32, Error> {
        let level = self
            .component
            .read_reg(self.interface, REGISTER_OFFSET_CBUFLVL)?;
        Ok(level * core::mem::size_of::<u32>() as u32)
    }

    /// Configure the capture stop-on-flush semantics.
    ///
    /// # Args
    /// * `stop` - Specified true if the capture should stop on flush events.
    pub fn stop_on_flush(&mut self, stop: bool) -> Result<(), Error> {
        let mut ffcr = FormatFlushControl::load(self.component, self.interface)?;
        ffcr.set_stoponfl(stop);
        ffcr.store(self.component, self.interface)?;
        Ok(())
    }

    /// Generate a manual flush event.
    pub fn manual_flush(&mut self) -> Result<(), Error> {
        let mut ffcr = FormatFlushControl::load(self.component, self.interface)?;
        ffcr.set_flushman(true);
        ffcr.store(self.component, self.interface)?;
        Ok(())
    }

    /// Get the size of the FIFO in bytes.
    pub fn fifo_size(&mut self) -> Result<u32, ArmError> {
        let size_words = self
            .component
            .read_reg(self.interface, REGISTER_OFFSET_RSZ)?;
        Ok(size_words * core::mem::size_of::<u32>() as u32)
    }
}

bitfield! {
    #[derive(Clone, Default)]
    pub struct FormatFlushControl(u32);
    impl Debug;

    pub drainbuf, set_drainbuf: 14;
    pub stpontrgev, set_stpontrgev: 13;
    pub stoponfl, set_stoponfl: 12;
    pub trigonfl, set_trigonfl: 10;
    pub trgontrgev, set_trgontrgev: 9;
    pub flushman, set_flushman: 6;
    pub fontrgev, set_fontrgev: 5;
    pub fonflin, set_flonflin: 4;
    pub enti, set_enti: 1;
    pub enft, set_enft: 0;
}

impl From<u32> for FormatFlushControl {
    fn from(raw: u32) -> Self {
        FormatFlushControl(raw)
    }
}

impl From<FormatFlushControl> for u32 {
    fn from(status: FormatFlushControl) -> u32 {
        status.0
    }
}

impl DebugRegister for FormatFlushControl {
    const ADDRESS: u32 = 0x304;
    const NAME: &'static str = "ETF_FFCR";
}

bitfield! {
    #[derive(Clone, Default)]
    pub struct Status(u32);
    impl Debug;

    pub empty, _: 4;
    pub ftempty, _: 3;
    pub ready, _: 2;
    pub trigd, _: 1;
    pub full, _: 0;
}

impl From<u32> for Status {
    fn from(raw: u32) -> Status {
        Status(raw)
    }
}

impl From<Status> for u32 {
    fn from(status: Status) -> u32 {
        status.0
    }
}

impl DebugRegister for Status {
    const ADDRESS: u32 = 0xC;
    const NAME: &'static str = "ETF_STS";
}

bitfield! {
    #[derive(Clone, Default)]
    pub struct EtfMode(u32);
    impl Debug;

    // The Mode register configures the operational mode of the FIFO.
    pub u8, mode, set_mode: 1, 0;
}

impl From<u32> for EtfMode {
    fn from(raw: u32) -> EtfMode {
        EtfMode(raw)
    }
}

impl From<EtfMode> for u32 {
    fn from(mode: EtfMode) -> u32 {
        mode.0
    }
}

impl DebugRegister for EtfMode {
    const ADDRESS: u32 = 0x28;
    const NAME: &'static str = "ETF_MODE";
}

/// Trace ID (a.k.a. ATID or trace source ID)
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(u8);
impl From<u8> for Id {
    fn from(id: u8) -> Self {
        Self(id)
    }
}
impl From<Id> for u8 {
    fn from(id: Id) -> Self {
        id.0
    }
}

/// Formatted frame demultiplexer.
/// Takes a reference to a 16 byte frame from the ETB/ETF or TPIU and
/// reads source ID and bytes from it.
#[derive(Copy, Clone, Debug)]
pub struct Frame<'a> {
    data: &'a [u8],
    idx: usize,
    id: Id,
}

impl<'a> Frame<'a> {
    pub fn new(data: &'a [u8], id: Id) -> Self {
        assert!(data.len() == 16);
        Self { data, id, idx: 0 }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn rewind(&mut self) {
        self.idx = 0;
    }
}

impl<'a> Iterator for &mut Frame<'a> {
    type Item = (Id, u8);

    fn next(&mut self) -> Option<Self::Item> {
        // DDI0314H_coresight_components_trm (ARM DDI 0314H) 9.6.1,
        // US20050039078A1,
        // and others
        if self.idx >= 15 {
            return None;
        }
        let byte = self.data[self.idx];
        let lsb = (self.data[15] >> (self.idx >> 1)) & 1;
        let ret = if self.idx & 1 != 0 {
            // Odd indices of the frame always contain data associated with the previous ID.
            Some((self.id, byte))
        } else if byte & 1 == 0 {
            // Even bytes utilize the LSbit to indicate if they contain an ID or data. For a cleared LSbit, data is
            // contained and the correct LSbit is stored at the end of the frame.
            Some((self.id, byte | lsb))
        } else {
            // Even bytes may also contain a new ID to swap to. In this case, the LSbit contained at the end of the
            // frame is used to indicate if the following data uses the new or old ID.
            let new_id = (byte >> 1).into();
            let next_id = if lsb == 1 { self.id } else { new_id };
            self.id = new_id;
            if self.idx >= 14 {
                None
            } else {
                self.idx += 1;
                Some((next_id, self.data[self.idx]))
            }
        };
        self.idx += 1;
        ret
    }
}
