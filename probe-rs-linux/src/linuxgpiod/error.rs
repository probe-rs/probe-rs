use probe_rs::probe::ProbeError;

/// Errors from the linuxgpiod probe driver.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum LinuxGpiodError {
    /// Probe selector is missing a serial (pin map).
    MissingSelector,

    /// Invalid pin selector: {0}
    InvalidSelector(String),

    /// Failed to request GPIO lines: {0}
    RequestLines(#[source] gpiocdev::Error),

    /// Failed to reconfigure GPIO lines: {0}
    Reconfigure(#[source] gpiocdev::Error),

    /// Failed to set GPIO line: {0}
    SetValue(#[source] gpiocdev::Error),

    /// Failed to read GPIO line: {0}
    GetValue(#[source] gpiocdev::Error),
}

impl ProbeError for LinuxGpiodError {}
