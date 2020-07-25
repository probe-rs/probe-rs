mod decoder;
mod publisher;

pub use decoder::{Decoder, TracePacket};
pub use publisher::{SwoPublisher, UpdaterChannel};

use crate::Error;

#[derive(Debug, Copy, Clone)]
pub enum SwoMode {
    UART,
    Manchester,
}

#[derive(Debug)]
pub struct SwoConfig {
    /// SWO mode: either UART or Manchester.
    mode: SwoMode,

    /// Baud rate of SWO, in Hz.
    ///
    /// This value is used to configure what baud rate the target
    /// generates and to configure what baud rate the probe receives,
    /// so must be a baud rate supported by both target and probe.
    baud: u32,

    /// Clock input to TPIU in Hz. This is often the system clock (HCLK/SYSCLK etc).
    tpiu_clk: u32,

    /// Whether to enable TPIU formatting.
    /// This is required to use ETM over SWO, but otherwise
    /// adds overhead if only DWT/ITM data is used.
    tpiu_continuous_formatting: bool,
}

impl SwoConfig {
    /// Create a new SwoConfig using the specified TPIU clock in Hz.
    ///
    /// By default the UART mode is used at 1MBd and
    /// TPIU continuous formatting is disabled (DWT/ITM only).
    pub fn new(tpiu_clk: u32) -> Self {
        SwoConfig {
            mode: SwoMode::UART,
            baud: 1_000_000,
            tpiu_clk: tpiu_clk,
            tpiu_continuous_formatting: false,
        }
    }

    /// Set the baud rate in Hz.
    pub fn set_baud(&mut self, baud: u32) -> &Self {
        self.baud = baud;
        self
    }

    /// Set the mode in this SwoConfig.
    pub fn set_mode(&mut self, mode: SwoMode) -> &Self {
        self.mode = mode;
        self
    }

    /// Set the TPIU continuous formatting setting.
    pub fn set_continuous_formatting(&mut self, enabled: bool) -> &Self {
        self.tpiu_continuous_formatting = enabled;
        self
    }

    pub fn mode(&self) -> SwoMode {
        self.mode
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }

    pub fn tpiu_clk(&self) -> u32 {
        self.tpiu_clk
    }

    pub fn tpiu_continuous_formatting(&self) -> bool {
        self.tpiu_continuous_formatting
    }
}

pub trait SwoAccess {
    /// Configure a SwoAccess interface for reading SWO data.
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), Error>;

    /// Disable SWO reading on this SwoAccess interface.
    fn disable_swo(&mut self) -> Result<(), Error>;

    /// Read any available SWO data without waiting.
    ///
    /// Returns a Vec<u8> of received SWO bytes since the last `read_swo()` call.
    /// If no data was available, returns an empty Vec.
    fn read_swo(&mut self) -> Result<Vec<u8>, Error> {
        self.read_swo_timeout(std::time::Duration::from_millis(10))
    }

    /// Read SWO data for up to `timeout` duration.
    ///
    /// If no data is received before the timeout, returns an empty Vec.
    /// May return earlier than `timeout` if the receive buffer fills up.
    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, Error>;

    /// Request an estimated best time to wait between polls of `read_swo`.
    ///
    /// A probe can implement this if it can work out a sensible time to
    /// wait between polls, for example using the probe's internal buffer
    /// size and SWO baud rate, or a 0s duration if reads can block for
    /// new data.
    ///
    /// The default implementation returns None.
    fn poll_interval_hint(&self) -> Option<std::time::Duration> {
        None
    }
}
