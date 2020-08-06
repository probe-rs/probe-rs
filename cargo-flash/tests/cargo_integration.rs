use cargo_flash::{self, ArtifactType, BuildType};

use std::path::{Path, PathBuf};

// Test reading metadata from
// the [package.metadata] section of
// Cargo.toml
#[test]
fn read_chip_metadata() {
    let work_dir = Path::new("tests/data/binary_project");

    let metadata = cargo_flash::read_metadata(work_dir).expect("Failed to read metadata.");

    assert_eq!(metadata.chip, Some("nrf51822".to_owned()));
}

#[test]
fn get_binary_artifact() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/binary_project");
    let expected_path =
        manifest_dir.join("tests/data/binary_project/target/release/binary_project");

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    )
    .expect("Failed to read artifact path.");

    assert_eq!(binary_path, expected_path);
}

#[test]
fn get_binary_artifact_with_cargo_config() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/binary_cargo_config");
    let expected_path = manifest_dir.join(
        "tests/data/binary_cargo_config/target/thumbv7m-none-eabi/release/binary_cargo_config",
    );

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    )
    .expect("Failed to read artifact path.");

    assert_eq!(binary_path, expected_path);
}

#[test]
fn get_binary_artifact_with_cargo_config_toml() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/binary_cargo_config_toml");
    let expected_path = manifest_dir.join(
        "tests/data/binary_cargo_config_toml/target/thumbv7m-none-eabi/release/binary_cargo_config_toml",
    );

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    )
    .expect("Failed to read artifact path.");

    //assert_eq!(binary_path, expected_path);
    //
    // Current cargo-flash will produce the wrong
    // path here, because '.cargo/config.toml' is not supported
    // by cargo-project. (See probe-rs/cargo-embed#29)
    assert_ne!(binary_path, expected_path);
}

#[test]
fn get_library_artifact_fails() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/library_project");

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    );

    // Currently, cargo-flash will try to flash the library, which does not work.
    // Should be fixed, so that an appropriate error message is shown.
    //
    // See issue #3.
    //
    // assert!(
    //     binary_path.is_err(),
    //     "Library project should not return a path to a binary, but got {}",
    //     binary_path.unwrap().display()
    // );

    assert!(binary_path.is_ok());
}

#[test]
fn workspace_root() {
    // In a workspace with a single binary crate,
    // we should be able to find the binary for that crate.

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/workspace_project");

    let _expected_path =
        manifest_dir.join("tests/data/workspace_project/target/release/workspace_bin");

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    );

    // Running cargo-flash in the workspace root is not yet supported,
    // and will cause an error. See issue #5.
    assert!(binary_path.is_err());

    // .expect("Failed to read artifact path.");

    // assert_eq!(binary_path, expected_path);
}

#[test]
fn workspace_binary_package() {
    // In a binary crate which is a member of a workspace,
    // we should be able to find the binary for that crate.

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/workspace_project/workspace_bin");

    let expected_path =
        manifest_dir.join("tests/data/workspace_project/target/release/workspace_bin");

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    )
    .expect("Failed to read artifact path.");

    assert_eq!(binary_path, expected_path);
}

#[test]
fn workspace_library_package() {
    // In a library crate which is a member of a workspace,
    // we should show an error message.

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let work_dir = manifest_dir.join("tests/data/workspace_project/workspace_lib");

    let binary_path = cargo_flash::get_artifact_path(
        &work_dir,
        BuildType::Release,
        None,
        ArtifactType::Unspecified,
    );

    // Currently, cargo-flash will try to flash the library, which does not work.
    // Should be fixed, so that an appropriate error message is shown.
    //
    // See issue #3.
    //
    // assert!(
    //     binary_path.is_err(),
    //     "Library project should not return a path to a binary, but got {}",
    //     binary_path.unwrap().display()
    // );

    assert!(binary_path.is_ok());
}
