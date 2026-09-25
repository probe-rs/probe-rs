use anyhow::Result;

use cargo_metadata::Message;
use serde::Deserialize;

use std::process::{Command, Stdio};

use std::path::{Path, PathBuf};
use thiserror::Error;

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
}

impl Artifact {
    /// Get the path of this output from the compiler.
    pub fn path(&self) -> &Path {
        &self.path
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

    let cargo_executable = std::env::var("CARGO");
    let cargo_executable = cargo_executable.as_deref().unwrap_or("cargo");

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
                    }

                    target_artifact = Some(artifact);
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
        })
    } else {
        // We did not find a binary, so we should return an error.
        Err(ArtifactError::NoArtifacts)
    }
}

/// Returns the cargo target triple from CLI, env, or hierarchical Cargo config.
///
/// Resolution order matches Cargo for `build.target`:
/// 1. Explicit `--target` (`target` argument)
/// 2. `CARGO_BUILD_TARGET` environment variable
/// 3. `build.target` in `.cargo/config{,.toml}`, walking from the current directory
///    up to the filesystem root, then `$CARGO_HOME`
pub fn cargo_target(target: Option<&str>) -> Option<String> {
    if let Some(target) = target {
        return Some(target.to_string());
    }

    if let Ok(target) = std::env::var("CARGO_BUILD_TARGET")
        && !target.is_empty()
    {
        return Some(target);
    }

    let cwd = std::env::current_dir().ok()?;
    build_target_from_cargo_config(&cwd)
}

fn build_target_from_cargo_config(start_dir: &Path) -> Option<String> {
    for dir in start_dir.ancestors() {
        if let Some(target) = read_build_target_from_cargo_dir(&dir.join(".cargo")) {
            return Some(target);
        }
    }

    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::UserDirs::new().map(|dirs| dirs.home_dir().join(".cargo")))?;

    read_build_target_from_cargo_dir(&cargo_home)
}

/// Prefer `.cargo/config` over `.cargo/config.toml` when both exist (Cargo behavior).
fn read_build_target_from_cargo_dir(cargo_dir: &Path) -> Option<String> {
    for name in ["config", "config.toml"] {
        let path = cargo_dir.join(name);
        if path.is_file() {
            return read_build_target(&path);
        }
    }
    None
}

fn read_build_target(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let config: CargoConfigFile = toml::from_str(&contents).ok()?;
    match config.build?.target? {
        BuildTarget::One(target) => Some(target),
        BuildTarget::Many(targets) => targets.into_iter().next(),
    }
}

#[derive(Debug, Deserialize)]
struct CargoConfigFile {
    build: Option<BuildTable>,
}

#[derive(Debug, Deserialize)]
struct BuildTable {
    target: Option<BuildTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BuildTarget {
    One(String),
    Many(Vec<String>),
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;

    #[test]
    fn read_build_target_string() {
        let dir = tempfile_dir("string_target");
        write_cargo_config(
            &dir,
            r#"
[build]
target = "thumbv7m-none-eabi"
"#,
        );

        assert_eq!(
            build_target_from_cargo_config(&dir).as_deref(),
            Some("thumbv7m-none-eabi")
        );
    }

    #[test]
    fn read_build_target_array() {
        let dir = tempfile_dir("array_target");
        write_cargo_config(
            &dir,
            r#"
[build]
target = ["thumbv7em-none-eabihf", "thumbv7m-none-eabi"]
"#,
        );

        assert_eq!(
            build_target_from_cargo_config(&dir).as_deref(),
            Some("thumbv7em-none-eabihf")
        );
    }

    #[test]
    fn prefers_nearest_config() {
        let root = tempfile_dir("nearest_root");
        write_cargo_config(
            &root,
            r#"
[build]
target = "thumbv6m-none-eabi"
"#,
        );

        let nested = root.join("nested");
        fs::create_dir_all(nested.join(".cargo")).unwrap();
        write_cargo_config(
            &nested,
            r#"
[build]
target = "thumbv7m-none-eabi"
"#,
        );

        assert_eq!(
            build_target_from_cargo_config(&nested).as_deref(),
            Some("thumbv7m-none-eabi")
        );
    }

    #[test]
    fn prefers_config_without_toml_extension() {
        let dir = tempfile_dir("config_pref");
        let cargo_dir = dir.join(".cargo");
        fs::create_dir_all(&cargo_dir).unwrap();
        fs::write(
            cargo_dir.join("config.toml"),
            r#"
[build]
target = "from-toml"
"#,
        )
        .unwrap();
        fs::write(
            cargo_dir.join("config"),
            r#"
[build]
target = "from-config"
"#,
        )
        .unwrap();

        assert_eq!(
            build_target_from_cargo_config(&dir).as_deref(),
            Some("from-config")
        );
    }

    #[test]
    fn cargo_target_explicit_overrides_config() {
        assert_eq!(
            cargo_target(Some("thumbv7m-none-eabi")).as_deref(),
            Some("thumbv7m-none-eabi")
        );
    }

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

    fn write_cargo_config(dir: &Path, contents: &str) {
        let cargo_dir = dir.join(".cargo");
        fs::create_dir_all(&cargo_dir).unwrap();
        fs::write(cargo_dir.join("config.toml"), contents).unwrap();
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "probe-rs-cargo-target-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
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
