//! SWO tracing related functions.

use crate::architecture::arm::communication_interface::ArmProbeInterface;

use super::ArmError;

/// The protocol the SWO pin should use for data transmission.
#[derive(Debug, Copy, Clone)]
pub enum SwoMode {
    /// UART
    Uart,
    /// Manchester
    Manchester,
}

/// The config for the SWO pin.
#[derive(Debug, Copy, Clone)]
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
            mode: SwoMode::Uart,
            baud: 1_000_000,
            tpiu_clk,
            tpiu_continuous_formatting: false,
        }
    }

    /// Set the baud rate in Hz.
    pub fn set_baud(mut self, baud: u32) -> Self {
        self.baud = baud;
        self
    }

    /// Set the mode in this SwoConfig.
    pub fn set_mode(mut self, mode: SwoMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the mode to UART
    pub fn set_mode_uart(mut self) -> Self {
        self.mode = SwoMode::Uart;
        self
    }

    /// Set the mode to Manchester
    pub fn set_mode_manchester(mut self) -> Self {
        self.mode = SwoMode::Manchester;
        self
    }

    /// Set the TPIU continuous formatting setting.
    pub fn set_continuous_formatting(mut self, enabled: bool) -> Self {
        self.tpiu_continuous_formatting = enabled;
        self
    }

    /// The SWO mode.
    pub fn mode(&self) -> SwoMode {
        self.mode
    }

    /// Baud rate of SWO, in Hz.
    ///
    /// This value is used to configure what baud rate the target generates
    /// and to configure what baud rate the probe receives,
    /// so must be a baud rate supported by both target and probe.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// Clock input to TPIU in Hz. This is often the system clock (HCLK/SYSCLK etc).
    pub fn tpiu_clk(&self) -> u32 {
        self.tpiu_clk
    }

    /// Whether to enable TPIU formatting. This is required to use ETM over
    /// SWO, but otherwise adds overhead if only DWT/ITM data is used.
    pub fn tpiu_continuous_formatting(&self) -> bool {
        self.tpiu_continuous_formatting
    }
}

/// An interface to operate SWO to be implemented on drivers that support SWO.
pub trait SwoAccess {
    /// Configure a SwoAccess interface for reading SWO data.
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError>;

    /// Disable SWO reading on this SwoAccess interface.
    fn disable_swo(&mut self) -> Result<(), ArmError>;

    /// Read any available SWO data without waiting.
    ///
    /// Returns a `Vec<u8>` of received SWO bytes since the last `read_swo()` call.
    /// If no data was available, returns an empty Vec.
    fn read_swo(&mut self) -> Result<Vec<u8>, ArmError> {
        self.read_swo_timeout(std::time::Duration::from_millis(10))
    }

    /// Read SWO data for up to `timeout` duration.
    ///
    /// If no data is received before the timeout, returns an empty Vec.
    /// May return earlier than `timeout` if the receive buffer fills up.
    fn read_swo_timeout(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>, ArmError>;

    /// Request an estimated best time to wait between polls of `read_swo`.
    ///
    /// A probe can implement this if it can work out a sensible time to
    /// wait between polls, for example using the probe's internal buffer
    /// size and SWO baud rate, or a 0s duration if reads can block for
    /// new data.
    ///
    /// The default implementation computes an estimated interval based on the buffer
    /// size, mode, and baud rate.
    fn swo_poll_interval_hint(&mut self, config: &SwoConfig) -> Option<std::time::Duration> {
        match self.swo_buffer_size() {
            Some(size) => poll_interval_from_buf_size(config, size),
            None => None,
        }
    }

    /// Request the probe SWO buffer size, if known.
    fn swo_buffer_size(&mut self) -> Option<usize> {
        None
    }
}

/// Helper function to compute a poll interval from a SwoConfig and SWO buffer size.
pub(crate) fn poll_interval_from_buf_size(
    config: &SwoConfig,
    buf_size: usize,
) -> Option<std::time::Duration> {
    let time_to_full_ms = match config.mode() {
        // In UART, the output data is at the baud rate with 10 clocks per byte.
        SwoMode::Uart => (1000 * buf_size as u32) / (config.baud() / 10),

        // In Manchester, the output data is at half the baud rate with
        // between 8.25 and 10 clocks per byte, so use a conservative 8 clocks/byte.
        SwoMode::Manchester => (500 * buf_size as u32) / (config.baud() / 8),
    };

    // Poll frequently enough to catch the buffer at 1/4 full
    Some(std::time::Duration::from_millis(time_to_full_ms as u64 / 4))
}

/// A reader interface to pull SWO data from the underlying driver.
pub struct SwoReader<'a> {
    interface: &'a mut dyn ArmProbeInterface,
    buf: Vec<u8>,
}

impl<'a> SwoReader<'a> {
    pub(crate) fn new(interface: &'a mut dyn ArmProbeInterface) -> Self {
        Self {
            interface,
            buf: Vec::new(),
        }
    }
}

impl<'a> std::io::Read for SwoReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use core::cmp;
        use std::{
            io::{Error, ErrorKind},
            mem,
        };

        // Always buffer: this pulls data as quickly as possible from
        // the target to clear it's embedded trace buffer, minimizing
        // the chance of an overflow event during which packets are
        // lost.
        self.buf.append(
            &mut self
                .interface
                .read_swo()
                .map_err(|e| Error::new(ErrorKind::Other, e))?,
        );

        let swo = {
            let next_buf = self.buf.split_off(cmp::min(self.buf.len(), buf.len()));
            mem::replace(&mut self.buf, next_buf)
        };

        buf[..swo.len()].copy_from_slice(&swo);
        Ok(swo.len())
    }
}
