# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added `nrf-recover` feature that unlocks nRF52 chips through Nordic's custom `AP`
- Added automatic CMSIS-Pack parsing and loading for flash algorithms.

### Changed

### Fixed


## [0.2.0]

Initial release on crates.io
- Added parsing of yaml (or anything else) config files for flash algorithm definitions, such that arbitrary chips can be added.
- Modularized code to allow other cores than M0 and be able to dynamically load chip definitions.
- Added target autodetection.
- Added M4 targets.

## [0.2.0]

- Working basic flash downloader with nRF51.
- Introduce cargo-flash which can automatically build & flash the target elf file.

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.2.0...master
[0.2.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0
