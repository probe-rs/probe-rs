use std::path::PathBuf;

// Test reading metadata from
// the [package.metadata] section of
// Cargo.toml
#[test]
fn read_chip_metadata() {
    let work_dir = test_project_dir("binary_project");

    let metadata = probe_rs_cli_util::read_metadata(&work_dir).expect("Failed to read metadata.");

    assert_eq!(metadata.chip, Some("nrf51822".to_owned()));
}

#[test]
fn get_binary_artifact() {
    let work_dir = test_project_dir("binary_project");
    let mut expected_path = work_dir.join("target");
    expected_path.push("debug");
    expected_path.push(host_binary_name("binary_project"));

    let args = [];

    let binary_artifact =
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

    assert_eq!(
        binary_artifact.path(),
        dunce::canonicalize(expected_path).expect("Failed to canonicalize path")
    );
}

#[test]
fn get_library_artifact_fails() {
    let work_dir = test_project_dir("library_project");

    let args = ["--release".to_owned()];

    let binary_artifact = probe_rs_cli_util::build_artifact(&work_dir, &args);

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to read artifact path.");

    assert_eq!(binary_artifact.path(), expected_path);
}

#[test]
fn workspace_library_package() {
    // In a library crate which is a member of a workspace,
    // we should show an error message.

    let work_dir = test_project_dir("workspace_project/workspace_lib");

    let args = ["--release".to_owned()];

    let binary_artifact = probe_rs_cli_util::build_artifact(&work_dir, &args);

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

    let binary_artifact = probe_rs_cli_util::build_artifact(&work_dir, &args);

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to get artifact path.");

    assert_eq!(binary_artifact.path(), expected_path);
}

#[test]
fn library_with_example() {
    // In a library with no binary target, but with an example,
    // we should return an error. (Same behaviour as cargo run)
    let work_dir = test_project_dir("library_with_example_project");

    let args = [];

    let binary_artifact = probe_rs_cli_util::build_artifact(&work_dir, &args);

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
        probe_rs_cli_util::build_artifact(&work_dir, &args).expect("Failed to get artifact path.");

    assert_eq!(binary_artifact.path(), expected_path);
}

/// Return the path to a test project, located in
/// tests/data.
fn test_project_dir(test_name: &str) -> PathBuf {
    let mut manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    manifest_dir.push("tests");
    manifest_dir.push("data");

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
