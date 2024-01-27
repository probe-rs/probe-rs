pub mod common_options;
pub mod flash;
pub mod logging;
pub mod rtt;

use anyhow::Result;

use cargo_metadata::Message;

use std::process::{Command, Stdio};

use std::path::PathBuf;
use std::{num::ParseIntError, path::Path};
use thiserror::Error;

pub fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(input)
}

pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
    parse_int::parse(input)
}

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
    #[error("Failed to run cargo build: exit code = {0:?}.")]
    CargoBuild(Option<i32>),
    #[error("Multiple binary artifacts were found.")]
    MultipleArtifacts,
    #[error("No binary artifacts were found.")]
    NoArtifacts,
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
    #[allow(unused)]
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

    tracing::debug!(
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
                    print!("{rendered}");
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn get_binary_artifact() {
        let work_dir = test_project_dir("binary_project");
        let mut expected_path = work_dir.join("target");
        expected_path.push("debug");
        expected_path.push(host_binary_name("binary_project"));

        let args = [];

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

        assert_eq!(binary_artifact.path(), expected_path);
    }

    #[test]
    fn get_binary_artifact_with_cargo_config() {
        let work_dir = test_project_dir("binary_cargo_config");

        let mut expected_path = work_dir.join("target");
        expected_path.push("thumbv7m-none-eabi");
        expected_path.push("debug");
        expected_path.push("binary_cargo_config");

        let args = [];

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

        assert_eq!(
            binary_artifact.path(),
            dunce::canonicalize(expected_path).expect("Failed to canonicalize path")
        );
    }

    #[test]
    fn get_binary_artifact_with_cargo_config_toml() {
        let work_dir = test_project_dir("binary_cargo_config_toml");
        let mut expected_path = work_dir.join("target");
        expected_path.push("thumbv7m-none-eabi");
        expected_path.push("debug");
        expected_path.push("binary_cargo_config_toml");

        let args = [];

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

        assert_eq!(
            binary_artifact.path(),
            dunce::canonicalize(expected_path).expect("Failed to canonicalize path")
        );
    }

    #[test]
    fn get_library_artifact_fails() {
        let work_dir = test_project_dir("library_project");

        let args = ["--release".to_owned()];

        let binary_artifact = build_artifact(&work_dir, &args);

        assert!(
            binary_artifact.is_err(),
            "Library project should not return a path to a binary, but got {}",
            binary_artifact.unwrap().path().display()
        );
    }

    #[test]
    fn workspace_root() {
        // In a workspace with a single binary crate,
        // we should be able to find the binary for that crate.

        let work_dir = test_project_dir("workspace_project");

        let mut expected_path = work_dir.join("target");
        expected_path.push("release");
        expected_path.push(host_binary_name("workspace_bin"));

        let args = owned_args(&["--release"]);

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

        assert_eq!(binary_artifact.path(), expected_path);
    }

    #[test]
    fn workspace_binary_package() {
        // In a binary crate which is a member of a workspace,
        // we should be able to find the binary for that crate.

        let workspace_dir = test_project_dir("workspace_project");
        let work_dir = workspace_dir.join("workspace_bin");

        let mut expected_path = workspace_dir.join("target");
        expected_path.push("release");
        expected_path.push(host_binary_name("workspace_bin"));

        let args = ["--release".to_owned()];

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

        assert_eq!(binary_artifact.path(), expected_path);
    }

    #[test]
    fn workspace_library_package() {
        // In a library crate which is a member of a workspace,
        // we should show an error message.

        let work_dir = test_project_dir("workspace_project/workspace_lib");

        let args = ["--release".to_owned()];

        let binary_artifact = build_artifact(&work_dir, &args);

        assert!(
            binary_artifact.is_err(),
            "Library project should not return a path to a binary, but got {}",
            binary_artifact.unwrap().path().display()
        );
    }

    #[test]
    fn multiple_binaries_in_crate() {
        // With multiple binaries in a crate,
        // we should show an error message if no binary is specified
        let work_dir = test_project_dir("multiple_binary_project");

        let args = [];

        let binary_artifact = build_artifact(&work_dir, &args);

        assert!(
            binary_artifact.is_err(),
            "With multiple binaries, an error message should be shown. Got path '{}' instead.",
            binary_artifact.unwrap().path().display()
        );
    }

    #[test]
    fn multiple_binaries_in_crate_select_binary() {
        // With multiple binaries in a crate,
        // we should show an error message if no binary is specified
        let work_dir = test_project_dir("multiple_binary_project");
        let mut expected_path = work_dir.join("target");
        expected_path.push("debug");
        expected_path.push(host_binary_name("bin_a"));

        let args = ["--bin".to_owned(), "bin_a".to_owned()];

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to get artifact path.");

        assert_eq!(binary_artifact.path(), expected_path);
    }

    #[test]
    fn library_with_example() {
        // In a library with no binary target, but with an example,
        // we should return an error. (Same behaviour as cargo run)
        let work_dir = test_project_dir("library_with_example_project");

        let args = [];

        let binary_artifact = build_artifact(&work_dir, &args);

        assert!(binary_artifact.is_err())
    }

    #[test]
    fn library_with_example_specified() {
        // When the example flag is specified, we should flash that example
        let work_dir = test_project_dir("library_with_example_project");
        let mut expected_path = work_dir.join("target");
        expected_path.push("debug");
        expected_path.push("examples");
        expected_path.push(host_binary_name("example"));

        let args = owned_args(&["--example", "example"]);

        let binary_artifact =
            build_artifact(&work_dir, &args).expect("Failed to get artifact path.");

        assert_eq!(binary_artifact.path(), expected_path);
    }

    /// Return the path to a test project, located in
    /// tests/data.
    fn test_project_dir(test_name: &str) -> PathBuf {
        let mut manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        manifest_dir.push("src");
        manifest_dir.push("bin");
        manifest_dir.push("probe-rs");
        manifest_dir.push("util");
        manifest_dir.push("test_data");

        manifest_dir.push(test_name);

        dunce::canonicalize(manifest_dir).expect("Failed to build canonicalized test_project_dir")
    }

    fn owned_args(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[cfg(not(windows))]
    fn host_binary_name(name: &str) -> String {
        name.to_string()
    }

    #[cfg(windows)]
    fn host_binary_name(name: &str) -> String {
        name.to_string() + ".exe"
    }
}
