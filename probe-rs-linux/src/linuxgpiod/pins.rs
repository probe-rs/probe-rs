use std::path::PathBuf;
use std::str::FromStr;

use gpiocdev::line::Offset;
use gpiocdev::request::Request;

use super::error::LinuxGpiodError;

/// Parsed pin assignments for a GPIO bit-bang SWD probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMap {
    /// Absolute path to the GPIO chip device, e.g. `/dev/gpiochip1`.
    pub chip: PathBuf,
    /// Line offset for SWCLK.
    pub swclk: Offset,
    /// Line offset for SWDIO.
    pub swdio: Offset,
    /// Optional line offset for SRST (active-low target reset).
    pub srst: Option<Offset>,
}

impl PinMap {
    /// Open the GPIO chip and request all configured lines as outputs.
    ///
    /// Initial states: SWCLK low (idle), SWDIO high (released), SRST high
    /// (target out of reset).
    pub fn request(&self) -> Result<Request, LinuxGpiodError> {
        let mut builder = Request::builder();
        builder
            .on_chip(&self.chip)
            .with_consumer("probe-rs")
            .with_line(self.swclk)
            .as_output(gpiocdev::line::Value::Inactive)
            .with_line(self.swdio)
            .as_output(gpiocdev::line::Value::Active);
        if let Some(srst) = self.srst {
            builder
                .with_line(srst)
                .as_output(gpiocdev::line::Value::Active);
        }
        builder.request().map_err(LinuxGpiodError::RequestLines)
    }
}

impl FromStr for PinMap {
    type Err = LinuxGpiodError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split(',');
        let chip_token = parts
            .next()
            .ok_or_else(|| LinuxGpiodError::InvalidSelector("empty selector".into()))?;
        let chip = parse_chip_path(chip_token)?;

        let mut swclk = None;
        let mut swdio = None;
        let mut srst = None;

        for part in parts {
            let (key, value) = part.split_once('=').ok_or_else(|| {
                LinuxGpiodError::InvalidSelector(format!("expected `key=offset`, got `{part}`"))
            })?;
            let offset: Offset = value.parse().map_err(|_| {
                LinuxGpiodError::InvalidSelector(format!(
                    "line offset for `{key}` must be a non-negative integer, got `{value}`"
                ))
            })?;
            match key {
                "swclk" => swclk = Some(offset),
                "swdio" => swdio = Some(offset),
                "srst" => srst = Some(offset),
                other => {
                    return Err(LinuxGpiodError::InvalidSelector(format!(
                        "unknown pin `{other}` (expected `swclk`, `swdio`, or `srst`)"
                    )));
                }
            }
        }

        let swclk = swclk
            .ok_or_else(|| LinuxGpiodError::InvalidSelector("missing required `swclk`".into()))?;
        let swdio = swdio
            .ok_or_else(|| LinuxGpiodError::InvalidSelector("missing required `swdio`".into()))?;

        if swclk == swdio || srst == Some(swclk) || srst == Some(swdio) {
            return Err(LinuxGpiodError::InvalidSelector(
                "pin offsets must be distinct".into(),
            ));
        }

        Ok(Self {
            chip,
            swclk,
            swdio,
            srst,
        })
    }
}

fn parse_chip_path(token: &str) -> Result<PathBuf, LinuxGpiodError> {
    if token.is_empty() {
        return Err(LinuxGpiodError::InvalidSelector("missing gpiochip".into()));
    }
    if token.starts_with('/') {
        return Ok(PathBuf::from(token));
    }
    if token.starts_with("gpiochip") {
        return Ok(PathBuf::from("/dev").join(token));
    }
    if token.chars().all(|c| c.is_ascii_digit()) {
        return Ok(PathBuf::from(format!("/dev/gpiochip{token}")));
    }
    Err(LinuxGpiodError::InvalidSelector(format!(
        "unrecognised gpiochip token `{token}` (expected `gpiochipN`, `/dev/gpiochipN`, or `N`)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parses_full_selector() {
        let pins: PinMap = "gpiochip1,swclk=26,swdio=25,srst=38".parse().unwrap();
        assert_eq!(pins.chip, Path::new("/dev/gpiochip1"));
        assert_eq!(pins.swclk, 26);
        assert_eq!(pins.swdio, 25);
        assert_eq!(pins.srst, Some(38));
    }

    #[test]
    fn parses_without_srst() {
        let pins: PinMap = "gpiochip0,swclk=4,swdio=5".parse().unwrap();
        assert_eq!(pins.chip, Path::new("/dev/gpiochip0"));
        assert_eq!(pins.swclk, 4);
        assert_eq!(pins.swdio, 5);
        assert_eq!(pins.srst, None);
    }

    #[test]
    fn parses_absolute_chip_path() {
        let pins: PinMap = "/dev/gpiochip2,swclk=10,swdio=11".parse().unwrap();
        assert_eq!(pins.chip, Path::new("/dev/gpiochip2"));
    }

    #[test]
    fn parses_numeric_chip_id() {
        let pins: PinMap = "3,swclk=1,swdio=2".parse().unwrap();
        assert_eq!(pins.chip, Path::new("/dev/gpiochip3"));
    }

    #[test]
    fn rejects_missing_swclk() {
        let err = "gpiochip0,swdio=5".parse::<PinMap>().unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }

    #[test]
    fn rejects_missing_swdio() {
        let err = "gpiochip0,swclk=4".parse::<PinMap>().unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }

    #[test]
    fn rejects_unknown_pin() {
        let err = "gpiochip0,swclk=4,swdio=5,trst=6"
            .parse::<PinMap>()
            .unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }

    #[test]
    fn rejects_duplicate_offsets() {
        let err = "gpiochip0,swclk=4,swdio=4".parse::<PinMap>().unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }

    #[test]
    fn rejects_non_numeric_offset() {
        let err = "gpiochip0,swclk=foo,swdio=5".parse::<PinMap>().unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }

    #[test]
    fn rejects_empty_selector() {
        let err = ",swclk=4,swdio=5".parse::<PinMap>().unwrap_err();
        assert!(matches!(err, LinuxGpiodError::InvalidSelector(_)));
    }
}
