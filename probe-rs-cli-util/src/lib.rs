pub mod argument_handling;
pub mod logging;

use anyhow::{anyhow, Context, Result};
use cargo_toml::Manifest;
use serde::Deserialize;

use cargo_metadata::Message;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub struct Metadata {
    pub chip: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Meta {
    pub chip: Option<String>,
}

pub fn read_metadata(work_dir: &Path) -> Result<Metadata> {
    let cargo_toml = work_dir.join("Cargo.toml");

    let cargo_toml_content = std::fs::read(&cargo_toml).context(format!(
        "Unable to read configuration file '{}'",
        cargo_toml.display(),
    ))?;

    let meta = match Manifest::<Meta>::from_slice_with_metadata(&cargo_toml_content) {
        Ok(m) => m.package.map(|p| p.metadata).flatten(),
        Err(_e) => None,
    };

    Ok(Metadata {
        chip: meta.and_then(|m| m.chip),
    })
}

/// Run `cargo build` and return the path to the generated binary artifact.
///
/// `args` will be passed to cargo build, and `--message-format json` will be
/// added to the list of arguments.
///
/// The output of `cargo build` is parsed to detect the path to the generated binary artifact.
/// If either no artifact, or more than a single artifact are created, an error is returned.
pub fn build_artifact(work_dir: &Path, args: &[String]) -> Result<PathBuf> {
    let work_dir = dunce::canonicalize(work_dir)
        .with_context(|| format!("Failed to canonicalize path {}", work_dir.display()))?;

    let cargo_executable = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());

    log::debug!(
        "Running '{}' in directory {}",
        cargo_executable,
        work_dir.display()
    );

    // Build the project.
    let cargo_command = Command::new(cargo_executable)
        .current_dir(work_dir)
        .arg("build")
        .args(args)
        .args(&["--message-format", "json-diagnostic-rendered-ansi"])
        .stdout(Stdio::piped())
        .spawn()?;

    let output = cargo_command.wait_with_output()?;

    // Parse build output.
    let messages = Message::parse_stream(&output.stdout[..]);

    // Find artifacts.
    let mut target_artifact = None;

    for message in messages {
        match message? {
            Message::CompilerArtifact(artifact) => {
                if artifact.executable.is_some() {
                    if target_artifact.is_some() {
                        // We found multiple binary artifacts,
                        // so we don't know which one to use.
                        return Err(anyhow!(
                            "Multiple binary artifacts found. \
                             Use '--bin' to specify which binary to flash."
                        ));
                    } else {
                        target_artifact = Some(artifact);
                    }
                }
            }
            Message::CompilerMessage(message) => {
                if let Some(rendered) = message.message.rendered {
                    print!("{}", rendered);
                }
            }
            // Ignore other messages.
            _ => (),
        }
    }

    // Check if the command succeeded, otherwise return an error.
    // Any error messages occuring during the build are shown above,
    // when the compiler messages are rendered.
    if !output.status.success() {
        // Show error output
        return Err(anyhow!(
            "Failed to run cargo build: exit code = {:?}",
            output.status.code()
        ));
    }

    if let Some(artifact) = target_artifact {
        // Unwrap is safe, we only store artifacts with an executable.
        Ok(artifact.executable.unwrap())
    } else {
        // We did not find a binary, so we should return an error.
        Err(anyhow!(
            "Unable to find any binary artifacts. \
                     Use '--example' to specify an example to flash, \
                     or '--package' to specify which package to flash in a workspace."
        ))
    }
}
