#![cfg(feature = "builtin-targets")]
use probe_rs::{Permissions, flashing::DownloadOptions, integration::FakeProbe, probe::Probe};

/// A chip where the flash algorithm's range is greater than the NVM range.
#[test]
fn flash_dry_run_stm32wb55ccux() {
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::with_mocked_core()));

    let mut session = probe
        .attach("stm32wb55ccux", Permissions::default())
        .expect("Failed to attach with 'fake' probe.");

    let mut flasher = session.target().flash_loader();

    flasher
        .add_data(0x8000000, &[0x1, 0x2, 0x3, 0x4])
        .expect("Failed to add flash");

    let mut flash_options = DownloadOptions::new();

    flash_options.dry_run = true;

    flasher
        .commit(&mut session, flash_options)
        .expect("Failed to flash in dry run mode.");
}

/// A chip where the flash algorithm's range could be less than the NVM range.
#[test]
fn flash_dry_run_mimxrt1010() {
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::with_mocked_core()));

    let mut session = probe
        .attach("mimxrt1010", Permissions::default())
        .expect("Failed to attach with 'fake' probe.");

    let mut flasher = session.target().flash_loader();

    flasher
        .add_data(0x60000000, &[0x1, 0x2, 0x3, 0x4])
        .expect("Failed to add flash");

    let mut flash_options = DownloadOptions::new();

    flash_options.dry_run = true;

    flasher
        .commit(&mut session, flash_options)
        .expect("Failed to flash in dry run mode.");
}

/// TLE987x: the Bootstrap Loader NAC/NAD word lives in the last 4 bytes of code
/// flash (0x11007ffc). Regression test for the IROM1/IROM2 boundary, which used
/// to sit at 0x11007ffc and so straddled the code-flash (`tle9871`) and EEPROM
/// (`tle9871_eep`) algorithm ranges, leaving no single algorithm able to cover
/// the region (`NoFlashLoaderAlgorithmAttached`).
#[test]
fn flash_dry_run_tle9871_nac_nad() {
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::with_mocked_core()));

    let mut session = probe
        .attach("TLE9871QXA20", Permissions::default())
        .expect("Failed to attach with 'fake' probe.");

    let mut flasher = session.target().flash_loader();

    // Vector table at flash origin + the NAC/NAD word at the very top of code flash.
    flasher
        .add_data(0x11000000, &[0x1, 0x2, 0x3, 0x4])
        .expect("Failed to add flash");
    flasher
        .add_data(0x11007ffc, &[0x1, 0xfe, 0x7f, 0x80])
        .expect("Failed to add flash");

    let mut flash_options = DownloadOptions::new();

    flash_options.dry_run = true;

    flasher
        .commit(&mut session, flash_options)
        .expect("Failed to flash in dry run mode.");
}
