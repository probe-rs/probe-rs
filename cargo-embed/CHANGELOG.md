# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

### Known issues

- Content that is longer than one line will not wrap when printed to the RTTUI unless it contains proper newlines itself.

## [0.8.0]

### Added

- Add Windows support with the help of crossterm instead of termion.

### Changed

### Fixed

### Known issues

- Content that is longer than one line will not wrap when printed to the RTTUI unless it contains proper newlines itself.

## [0.7.0]

### Changed

- Improve error handling a lot. We now print the complete stack of errors with anyhow/thiserror.
- Update to the probe-rs 0.7.0 API.

### Fixed

- Fixed a bug where cargo-embed would always flash the attached chip no matter if enabled or not.

### Known issues

- Content that is longer than one line will not wrap when printed to the RTTUI unless it contains proper newlines itself.

## [0.6.1]

### Added

- Added the possibility to use an `Embed.local.toml` to override the `Embed.toml` locally.
- Added the possibility to use an `.embed.toml` and `.embed.local.toml`. See the [README](README.md) for more info.
- Added host timestamps to the RTT printouts.

## [0.6.0]
- Initial release

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.8.0...master
[0.8.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.8.0..v0.7.0
[0.7.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.7.0..v0.6.1
[0.6.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.1..v0.6.0
[0.6.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.0