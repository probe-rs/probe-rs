pub mod logging;

use anyhow::{anyhow, Context, Result};
use cargo_toml::Manifest;
use serde::Deserialize;

use std::path::{Path, PathBuf};

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

pub enum BuildType {
    Debug,
    Release,
}

pub enum ArtifactType {
    Binary(String),
    Example(String),
    Unspecified,
}

pub fn get_artifact_path(
    work_dir: &Path,
    build_type: BuildType,
    target: Option<&str>,
    artifact_type: ArtifactType,
) -> Result<PathBuf> {
    // Try and get the cargo project information.
    let project = cargo_project::Project::query(work_dir)
        .map_err(|e| anyhow!("failed to parse Cargo project information: {}", e))?;

    // Decide what artifact to use.
    let artifact = match artifact_type {
        ArtifactType::Binary(ref name) => cargo_project::Artifact::Bin(name),
        ArtifactType::Example(ref name) => cargo_project::Artifact::Example(name),
        ArtifactType::Unspecified => cargo_project::Artifact::Bin(project.name()),
    };

    // Decide what profile to use.
    let profile = match build_type {
        BuildType::Release => cargo_project::Profile::Release,
        BuildType::Debug => cargo_project::Profile::Dev,
    };

    // Try and get the artifact path.
    project
        .path(artifact, profile, target, "x86_64-unknown-linux-gnu")
        .map_err(|e| anyhow!("Couldn't get artifact path: {}", e))
}
