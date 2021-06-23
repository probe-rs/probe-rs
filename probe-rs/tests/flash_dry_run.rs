use probe_rs::{flashing::DownloadOptions, FakeProbe, Probe};

#[test]
fn flash_dry_run() {
    let probe = Probe::from_specific_probe(Box::new(FakeProbe::new()));

    let mut session = probe
        .attach("stm32wb55ccux")
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
