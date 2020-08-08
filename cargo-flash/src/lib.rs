pub mod logging;

use anyhow::{anyhow, Context, Result};
use cargo_toml::Manifest;
use serde::Deserialize;

use cargo_metadata::Message;
use std::{
    io::BufReader,
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

pub fn get_artifact_path(work_dir: &Path, args: &[String]) -> Result<PathBuf> {
    let cargo_executable = std::env::var("CARGO").unwrap_or("cargo".to_owned());

    // Build the project
    let mut cargo_command = Command::new(cargo_executable)
        .current_dir(work_dir)
        .arg("build")
        .args(args)
        .args(&["--message-format", "json"])
        .stdout(Stdio::piped())
        .spawn()?;

    let status = cargo_command.wait()?;

    if !status.success() {
        handle_failed_command(status)
    }

    let reader = BufReader::new(cargo_command.stdout.unwrap());

    // parse build output
    let messages = Message::parse_stream(reader);

    // find artifacts
    let mut target_artifact = None;

    for message in messages {
        match message? {
            Message::CompilerArtifact(artifact) => {
                if artifact.executable.is_some() {
                    if target_artifact.is_some() {
                        // We found multiple binary artifacts,
                        // so we don't know which one to use.
                        return Err(anyhow!("Multiple binary artifacts found."));
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
            // Ignore other messages
            _ => (),
        }
    }

    if let Some(artifact) = target_artifact {
        // Unwrap is safe, we only store artifacts with an executable
        Ok(artifact.executable.unwrap())
    } else {
        // We did not find a binary, so we should return an error
        Err(anyhow!("Unable to find binary artifact."))
    }
}

#[cfg(unix)]
fn handle_failed_command(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    let status = status.code().or_else(|| status.signal()).unwrap_or(1);
    std::process::exit(status)
}

#[cfg(not(unix))]
fn handle_failed_command(status: std::process::ExitStatus) -> ! {
    let status = status.code().unwrap_or(1);
    std::process::exit(status)
}
