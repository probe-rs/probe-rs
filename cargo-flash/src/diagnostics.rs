/// Error handling
use colored::*;
use std::fmt::{Display, Write};

use probe_rs::{
    config::MemoryRegion, config::TargetDescriptionSource, flashing::FileDownloadError, Target,
};

#[derive(Debug)]
pub struct Diagnostic {
    error: anyhow::Error,

    hints: Vec<String>,
}

impl Diagnostic {
    pub fn render(&self) {
        use std::io::Write;

        let mut stderr = std::io::stderr();

        let err_msg = format!("{:?}", self.error);

        let _ = write_with_offset(&mut stderr, "Error".red().bold(), &err_msg);

        let _ = writeln!(stderr, "");

        for hint in &self.hints {
            write_with_offset(&mut stderr, "Hint".blue().bold(), hint);
        }

        let _ = stderr.flush();
    }

    pub fn source_error(&self) -> &anyhow::Error {
        &self.error
    }

    pub fn add_hint(&mut self, hint: impl Into<String>) {
        self.hints.push(hint.into())
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl From<anyhow::Error> for Diagnostic {
    fn from(error: anyhow::Error) -> Self {
        Diagnostic {
            error,
            hints: vec![],
        }
    }
}

pub fn handle_flash_error(err: anyhow::Error, target: &Target) -> Result<(), Diagnostic> {
    // Try to get a probe_rs::Error out of the anyhow error

    let hints = if let Some(err) = err.downcast_ref::<FileDownloadError>() {
        handle_probe_rs_error(err, target)
    } else {
        vec![]
    };

    let mut diagnostic: Diagnostic = err.into();

    diagnostic.hints = hints;

    Err(diagnostic)
}

fn handle_probe_rs_error(err: &FileDownloadError, target: &Target) -> Vec<String> {
    match err {
        FileDownloadError::Flash(probe_rs::flashing::FlashError::NoSuitableFlash {
            start: _,
            end: _,
            description_source,
        }) => {
            if &TargetDescriptionSource::Generic == description_source {
                return vec![
                    "A generic chip was selected as the target. For flashing, it is necessary to specify a concrete chip.\n\
                    Use `--list-chips` to see all available chips.".to_owned()
                    ];
            }

            let mut buff = String::new();

            // Show the available flash regions
            let _ = writeln!(
                buff,
                "The following flash memory is available for the chip '{:?}':",
                target.identifier
            );

            for memory_region in &target.memory_map {
                match memory_region {
                    MemoryRegion::Ram(_) => {}
                    MemoryRegion::Generic(_) => {}
                    MemoryRegion::Flash(flash) => {
                        let _ = writeln!(
                            buff,
                            "  {:#08x} - {:#08x}",
                            &flash.range.start, flash.range.end
                        );
                    }
                }
            }

            return vec![buff];
        }
        // Ignore other errors
        _ => (),
    }

    vec![]
}

fn write_with_offset(mut output: impl std::io::Write, header: ColoredString, msg: &str) {
    let _ = write!(output, "       {} ", header);

    let mut lines = msg.lines();

    if let Some(first_line) = lines.next() {
        let _ = writeln!(output, "{}", first_line);
    }

    for line in lines {
        let _ = writeln!(output, "            {}", line);
    }
}
