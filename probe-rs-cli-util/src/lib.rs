pub mod common_options;
pub mod flash;
pub mod logging;
pub mod meta;
pub mod rtt;

use cargo_toml::Manifest;
use serde::Deserialize;
use thiserror::Error;

use cargo_metadata::Message;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

// Re-export crates to avoid version conflicts in the dependent crates.
pub use clap;
pub use indicatif;
pub use log;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("Failed to canonicalize path '{work_dir}'.")]
    Canonicalize {
        #[source]
        source: std::io::Error,
        work_dir: String,
    },
    #[error("An IO error occurred during the execution of 'cargo build'.")]
    Io(#[source] std::io::Error),
    #[error("Unable to read the Cargo.toml at '{path}'.")]
    CargoToml {
        #[source]
        source: std::io::Error,
        path: String,
    },
    #[error("Failed to run cargo build: exit code = {0:?}.")]
    CargoBuild(Option<i32>),
    #[error("Multiple binary artifacts were found.")]
    MultipleArtifacts,
    #[error("No binary artifacts were found.")]
    NoArtifacts,
}

pub struct Metadata {
    pub chip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Meta {
    pub chip: Option<String>,
}

pub fn read_metadata(work_dir: &Path) -> Result<Metadata, ArtifactError> {
    let cargo_toml = work_dir.join("Cargo.toml");

    let cargo_toml_content = std::fs::read(&cargo_toml).map_err(|e| ArtifactError::CargoToml {
        source: e,
        path: format!("{}", cargo_toml.display()),
    })?;

    let meta = match Manifest::<Meta>::from_slice_with_metadata(&cargo_toml_content) {
        Ok(m) => m.package.and_then(|p| p.metadata),
        Err(_e) => None,
    };

    Ok(Metadata {
        chip: meta.and_then(|m| m.chip),
    })
}

/// Represents compiled code that the compiler emitted during compilation.
pub struct Artifact {
    path: PathBuf,
    fresh: bool,
}

impl Artifact {
    /// Get the path of this output from the compiler.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// If `true`, then the artifact was unchanged during compilation.
    pub fn fresh(&self) -> bool {
        self.fresh
    }
}

/// Run `cargo build` and return the generated binary artifact.
///
/// `args` will be passed to cargo build, and `--message-format json` will be
/// added to the list of arguments.
///
/// The output of `cargo build` is parsed to detect the path to the generated binary artifact.
/// If either no artifact, or more than a single artifact are created, an error is returned.
pub fn build_artifact(work_dir: &Path, args: &[String]) -> Result<Artifact, ArtifactError> {
    let work_dir = dunce::canonicalize(work_dir).map_err(|e| ArtifactError::Canonicalize {
        source: e,
        work_dir: format!("{}", work_dir.display()),
    })?;

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
        .args(["--message-format", "json-diagnostic-rendered-ansi"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(ArtifactError::Io)?;

    let output = cargo_command
        .wait_with_output()
        .map_err(ArtifactError::Io)?;

    // Parse build output.
    let messages = Message::parse_stream(&output.stdout[..]);

    // Find artifacts.
    let mut target_artifact = None;

    for message in messages {
        match message.map_err(ArtifactError::Io)? {
            Message::CompilerArtifact(artifact) => {
                if artifact.executable.is_some() {
                    if target_artifact.is_some() {
                        // We found multiple binary artifacts,
                        // so we don't know which one to use.
                        return Err(ArtifactError::MultipleArtifacts);
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
    // Any error messages occurring during the build are shown above,
    // when the compiler messages are rendered.
    if !output.status.success() {
        return Err(ArtifactError::CargoBuild(output.status.code()));
    }

    if let Some(artifact) = target_artifact {
        // Unwrap is safe, we only store artifacts with an executable.
        Ok(Artifact {
            path: PathBuf::from(artifact.executable.unwrap().as_path()),
            fresh: artifact.fresh,
        })
    } else {
        // We did not find a binary, so we should return an error.
        Err(ArtifactError::NoArtifacts)
    }
}
