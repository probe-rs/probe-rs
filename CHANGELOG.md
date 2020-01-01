# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

## [0.3.0]

Improved flashing for `cargo-flash` considering speed and useability.

### Added

- Increased the raw flashing speed by factor 10 and the actual flashing speed for small programs by factor 5. This is done using batched CMSIS-DAP transfers.
- Added CMSIS-Pack powered flashing. This feature essentially enables to flash any ARM core which can also be flashed by ARM Keil.
- Added progress bars for flash progress indication.
- Added `nrf-recover` feature that unlocks nRF52 chips through Nordic's custom `AP`

### Changed

- Improved target autodetection with better error distinction.
- Improved messaging overall.

### Fixed

- Various bugfixes
- Binaries bigger than a sector can now be flashed.

## [0.2.0]

Initial release on crates.io
- Added parsing of yaml (or anything else) config files for flash algorithm definitions, such that arbitrary chips can be added.
- Modularized code to allow other cores than M0 and be able to dynamically load chip definitions.
- Added target autodetection.
- Added M4 targets.
- Working basic flash downloader with nRF51.
- Introduce cargo-flash which can automatically build & flash the target elf file.

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.2.0...master
[0.3.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.3.0
[0.2.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0
