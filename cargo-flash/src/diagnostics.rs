/// Error handling
use colored::*;
use std::fmt::{Display, Write};

use bytesize::ByteSize;

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
            let _ = writeln!(stderr, "");
        }

        let _ = stderr.flush();
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

pub fn handle_flash_error(
    err: anyhow::Error,
    target: &Target,
    target_spec: Option<&str>,
) -> Diagnostic {
    // Try to get a probe_rs::Error out of the anyhow error

    let hints = if let Some(err) = err.downcast_ref::<FileDownloadError>() {
        handle_probe_rs_error(err, target, target_spec)
    } else {
        vec![]
    };

    let mut diagnostic: Diagnostic = err.into();

    diagnostic.hints = hints;

    diagnostic
}

fn handle_probe_rs_error(
    err: &FileDownloadError,
    target: &Target,
    target_spec: Option<&str>,
) -> Vec<String> {
    match err {
        FileDownloadError::Flash(probe_rs::flashing::FlashError::NoSuitableNvm {
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

            let mut hints = Vec::new();

            let mut hint_available_regions = String::new();

            // Show the available flash regions
            let _ = writeln!(
                hint_available_regions,
                "The following flash memory is available for the chip '{}':",
                target.name
            );

            for memory_region in &target.memory_map {
                match memory_region {
                    MemoryRegion::Ram(_) => {}
                    MemoryRegion::Generic(_) => {}
                    MemoryRegion::Nvm(flash) => {
                        let _ = writeln!(
                            hint_available_regions,
                            "  {:#010x} - {:#010x} ({})",
                            flash.range.start,
                            flash.range.end,
                            ByteSize((flash.range.end - flash.range.start) as u64)
                                .to_string_as(true)
                        );
                    }
                }
            }

            hints.push(hint_available_regions);

            if let Some(target_spec) = target_spec {
                // Check if the chip specification was unique
                let matching_chips = probe_rs::config::search_chips(target_spec).unwrap();

                log::info!(
                    "Searching for all chips for spec '{}', found {}",
                    target_spec,
                    matching_chips.len()
                );

                if matching_chips.len() > 1 {
                    let mut non_unique_target_hint = format!("The specified chip '{}' did match multiple possible targets. Try to specify your chip more exactly. The following possible targets were found:\n", target_spec);

                    for target in matching_chips {
                        non_unique_target_hint.push_str(&format!("\t{}\n", target));
                    }

                    hints.push(non_unique_target_hint)
                }
            }

            return hints;
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
