# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

## [0.10.2]

### Changed

- Disable sentry by default as an intermediate measure to fix the subpar user experience due to its introduction.

## [0.10.0]

### Added

- Updated to probe-rs 0.10.0. Please consult its own changelog for new features and fixes.
- Added logging to sentry.io. This is 100% OPT-IN! You will be asked only if an unhandled error or panic occurs, and only if you give consent, data is transmitted. If you do not trust us either way, you can disable the `sentry` feature when you install the crate. The completely anonymous data can be investigated on sentry.io by anyone who likes to see it. Unfortunately sentry.io does not feature public orgs yet, so please reach out to @Yatekii to be added.
Sentry helps us track down tricky issues that only occur in very specific cases. It is very much appreciated if you log upcoming errors this way (#121)!

### Changed

- FTDI support is now optional. To enable FTDI support, please use the `ftdi` feature (#124).

## [0.9.0]

### Added

- Support for cargo workspaces was added with the integration of `cargo-metadata` instead of `cargo-project` (in #39 by @Tiwalun)
- Show the compiler output in `cargo flash` if the called `cargo build` command fails (in #53 by @Tiwalun).

### Changed

### Fixed

### Removed

- The option to start a GDB server after flashing is removed. It is recommended to use [cargo-embed](https://github.com/probe-rs/cargo-embed)
  to start a GDB server. The following options are removed:
  - `--gdb`
  - `--no-download`
  - `--gdb-connection-string`

## [0.8.0]

### Added

- Added `Cargo.toml` metadata parsing for specifying the chip (see https://github.com/probe-rs/cargo-flash/pull/31).
- Probes can now be selected via the VID:PID:[SerialNo] triplet.

### Changed

- Improved error logging by a large margin! Errors are now displayed properly in stacked fashion and are easier to read.
- Cleaned up some of the logging output. Mostly beauty stuff.

### Fixed

## [0.7.0]

### Added

### Changed

### Fixed

## [0.6.0]

### Added

- Add a `--speed` setting to configure protocol speed in kHz.
- Upgrade to probe-rs 0.6.0 which fixes some bugs that appeared within cargo-flash (see [CHANGELOG](https://github.com/probe-rs/probe-rs/blob/master/CHANGELOG.md))
- Add a `--restore-unwritten` flag which makes the flashing procedure restore all bytes that have been erased in the sectore erase but are not actually in the writeable sections of the ELF data.
- Add an `--elf` setting to point to a specific ELF binary instead of a cargo one.
- Add a `--work-dir` for cargo flash to operate in.

## [0.5.0]

### Added

- Adds support for JLink and JTag based flashing.
- Add the possibility to select the debug protocol (SWD/JTAG) with `--protocol`.
- Added the possibility to set the log level via the `--log` argument.

### Changed

### Fixed

- Fix a bug where `--probe-index` would be handed to cargo build accidentially.
- Logs are now always shown, even with progressbars enabled.
  Before progressbars would behave weirdly and errors would not be shown.
  Now this is handled properly and any output is shown above the progress bars.

### Known issues

- Some chips do not reset automatically after flashing
- The STM32L0 cores have issues with flashing.

## [0.4.0]

### Added

- A basic GDB server was added \o/ You can either use the provided `gdb-server` binary or use `cargo flash --gdb` to first flash the target and then open a GDB session. There is many more new options which you can list with `cargo flash --help`.
- A flag to disable progressbars was added. Error reporting was broken because of progressbar overdraw. Now one can disable progress bars to see errors. In the long run this has to be fixed.

### Changed

### Fixed

## [0.3.0]

Improved flashing for `cargo-flash` considering speed and useability.

### Added

- Added CMSIS-Pack powered flashing. This feature essentially enables to flash any ARM core which can also be flashed by ARM Keil.
- Added progress bars for flash progress indication.
- Added `nrf-recover` feature that unlocks nRF52 chips through Nordic's custom `AP`

### Changed

### Fixed

- Various bugfixes

## [0.2.0]
- Introduce cargo-flash which can automatically build & flash the target elf file.

[Unreleased]: https://github.com/probe-rs/cargo-flash/compare/v0.10.2...master
[0.10.2]: https://github.com/probe-rs/cargo-flash/releases/tag/v0.10.1..v0.10.2
[0.10.1]: https://github.com/probe-rs/cargo-flash/releases/tag/v0.9.0..v0.10.1
[0.9.0]: https://github.com/probe-rs/cargo-flash/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/probe-rs/cargo-flash/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/probe-rs/cargo-flash/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/probe-rs/cargo-flash/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/probe-rs/cargo-flash/releases/tag/v0.5.0
[0.4.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.4.0
[0.3.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.3.0
[0.2.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0
