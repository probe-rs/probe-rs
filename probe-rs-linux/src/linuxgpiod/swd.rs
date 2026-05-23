use gpiocdev::line::{Offset, Value};
use gpiocdev::request::{Config, Request};

use probe_rs::probe::IoSequenceItem;

use super::error::LinuxGpiodError;

pub struct SwdBus {
    request: Request,
    swclk: Offset,
    swdio: Offset,
    srst: Option<Offset>,
    swdio_dir: SwdioDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwdioDir {
    Output,
    Input,
}

impl SwdBus {
    pub fn new(request: Request, swclk: Offset, swdio: Offset, srst: Option<Offset>) -> Self {
        Self {
            request,
            swclk,
            swdio,
            srst,
            swdio_dir: SwdioDir::Output,
        }
    }

    /// Returns one bool per item: the driven bit for `Output`, the sampled
    /// SWDIO state for `Input`.
    pub fn transfer<I>(&mut self, items: I) -> Result<Vec<bool>, LinuxGpiodError>
    where
        I: IntoIterator<Item = IoSequenceItem>,
    {
        let items: Vec<IoSequenceItem> = items.into_iter().collect();
        let mut result = Vec::with_capacity(items.len());
        for item in items {
            match item {
                IoSequenceItem::Output(bit) => {
                    self.ensure_dir(SwdioDir::Output)?;
                    self.write_bit(bit)?;
                    result.push(bit);
                }
                IoSequenceItem::Input => {
                    self.ensure_dir(SwdioDir::Input)?;
                    result.push(self.read_bit()?);
                }
            }
        }
        Ok(result)
    }

    /// Returns `false` if SRST is not routed.
    pub fn drive_srst(&self, asserted_high: bool) -> Result<bool, LinuxGpiodError> {
        let Some(srst) = self.srst else {
            return Ok(false);
        };
        let value = if asserted_high {
            Value::Active
        } else {
            Value::Inactive
        };
        self.request
            .set_value(srst, value)
            .map_err(LinuxGpiodError::SetValue)?;
        Ok(true)
    }

    fn ensure_dir(&mut self, dir: SwdioDir) -> Result<(), LinuxGpiodError> {
        if self.swdio_dir == dir {
            return Ok(());
        }
        let mut cfg = Config::default();
        cfg.with_line(self.swclk).as_output(Value::Inactive);
        match dir {
            SwdioDir::Output => {
                cfg.with_line(self.swdio).as_output(Value::Active);
            }
            SwdioDir::Input => {
                cfg.with_line(self.swdio).as_input();
            }
        }
        if let Some(srst) = self.srst {
            cfg.with_line(srst).as_output(Value::Active);
        }
        self.request
            .reconfigure(&cfg)
            .map_err(LinuxGpiodError::Reconfigure)?;
        self.swdio_dir = dir;
        Ok(())
    }

    fn write_bit(&self, bit: bool) -> Result<(), LinuxGpiodError> {
        // SWDIO must be stable across the rising edge: clock low, then data.
        self.request
            .set_value(self.swclk, Value::Inactive)
            .map_err(LinuxGpiodError::SetValue)?;
        let val = if bit { Value::Active } else { Value::Inactive };
        self.request
            .set_value(self.swdio, val)
            .map_err(LinuxGpiodError::SetValue)?;
        self.request
            .set_value(self.swclk, Value::Active)
            .map_err(LinuxGpiodError::SetValue)?;
        Ok(())
    }

    fn read_bit(&self) -> Result<bool, LinuxGpiodError> {
        // Sample AFTER the rising edge — the polyfill assumes the
        // "turnaround" input slot already contains the first ACK bit.
        self.request
            .set_value(self.swclk, Value::Inactive)
            .map_err(LinuxGpiodError::SetValue)?;
        self.request
            .set_value(self.swclk, Value::Active)
            .map_err(LinuxGpiodError::SetValue)?;
        let value = self
            .request
            .value(self.swdio)
            .map_err(LinuxGpiodError::GetValue)?;
        Ok(matches!(value, Value::Active))
    }
}
