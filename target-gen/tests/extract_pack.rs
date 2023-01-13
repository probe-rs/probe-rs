use assert_cmd::Command;

const NORDIC_SAMPLE_PACK: &str =
    "tests/test_data/NordicSemiconductor.nRF_DeviceFamilyPack.8.32.1.pack";

#[test]
fn missing_output_directory() {
    let mut cmd = Command::cargo_bin("target-gen").unwrap();

    // extract an example pack
    cmd.arg("pack").arg(NORDIC_SAMPLE_PACK);

    cmd.assert().failure().stderr(predicates::str::contains(
        "the following required arguments were not provided:",
    ));
}

#[test]
fn extract_target_specs() {
    // create a temporary directory
    let temp = assert_fs::TempDir::new().unwrap();

    let mut cmd = Command::cargo_bin("target-gen").unwrap();

    // extract an example pack
    cmd.arg("pack").arg(NORDIC_SAMPLE_PACK).arg(temp.path());

    cmd.assert().success().stdout(predicates::str::contains(
        "Generated 4 target definition(s):",
    ));
}
