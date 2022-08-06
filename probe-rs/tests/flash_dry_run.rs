use probe_rs::{flashing::DownloadOptions, FakeProbe, Permissions, Probe};

/// A chip where the flash algorithm's range is greater than the NVM range.
#[test]
fn flash_dry_run_stm32wb55ccux() {
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::new()));

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
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::new()));

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
