# Change Log

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/)
and this project adheres to [Semantic Versioning](http://semver.org/).

## [Unreleased]

### Added

- [#30] added a `--no-flash` flag to run a program without re-flashing it
- [#40] added (`--help`) documentation to the many CLI flags
- [#33] added canary-based stack overflow detection
- [#38] added file location information to log messages
- [#41] the `PROBE_RUN_CHIP` env variable can be used as an alternative to the `--chip` flag
- [#49] `--list-probes` and `--probe` flags to list all probes and select a particular probe, respectively
- [#55] added more precise stack overflow detection for apps linked with `flip-link`
- [#57] added module location information to log messages
- [#63] added file location information to the stack backtrace
- [#83] added git info to the `--version` output
- [#88] added `--speed` flag to set the frequency of the probe

[#30]: https://github.com/knurling-rs/probe-run/pull/30
[#33]: https://github.com/knurling-rs/probe-run/pull/33
[#38]: https://github.com/knurling-rs/probe-run/pull/38
[#40]: https://github.com/knurling-rs/probe-run/pull/40
[#41]: https://github.com/knurling-rs/probe-run/pull/41
[#49]: https://github.com/knurling-rs/probe-run/pull/49
[#55]: https://github.com/knurling-rs/probe-run/pull/55
[#57]: https://github.com/knurling-rs/probe-run/pull/57
[#63]: https://github.com/knurling-rs/probe-run/pull/63
[#83]: https://github.com/knurling-rs/probe-run/pull/83
[#88]: https://github.com/knurling-rs/probe-run/pull/88

### Fixed

- [#28] notify the user ASAP that RTT logs were not found in the image
- [#50] fixed a bug that was causing an infinite stack backtrace to be printed
- [#51] fixed the handling of Ctrl-C
- [#77] flush stdout after each write; fixes a bug where output was not printed until a newline was sent from the device using non-defmt-ed RTT

[#28]: https://github.com/knurling-rs/probe-run/pull/28
[#50]: https://github.com/knurling-rs/probe-run/pull/50
[#51]: https://github.com/knurling-rs/probe-run/pull/51
[#77]: https://github.com/knurling-rs/probe-run/pull/77

### Changed

- [#25] increased RTT attach retries, which is sometimes needed for inter-operation with `rtt-target`
- [#44] improve diagnostics when linker script is missing
- [#53], [#60] the output format of logs
- [#55], [#64] all hard faults make `probe-run` exit with non-zero exit code regardless of whether `panic-probe` was used or not
- [#69] `probe-run` now changes the RTT mode to blocking at runtime, right after RAM initialization

[#25]: https://github.com/knurling-rs/probe-run/pull/25
[#44]: https://github.com/knurling-rs/probe-run/pull/44
[#53]: https://github.com/knurling-rs/probe-run/pull/53
[#55]: https://github.com/knurling-rs/probe-run/pull/55
[#60]: https://github.com/knurling-rs/probe-run/pull/60
[#64]: https://github.com/knurling-rs/probe-run/pull/64
[#69]: https://github.com/knurling-rs/probe-run/pull/69

## [v0.1.3] - 2020-08-19

### Changed

- Fixed outdated comment in readme

## [v0.1.2] - 2020-08-19

### Added

- Support for the `thumbv7em-none-eabihf` target.

### Changed

- Bumped the `probe-rs` dependency to 0.8.0
- Cleaned up CLI documentation

## [v0.1.1] - 2020-08-17

### Added

- Added setup instructions to check that there's enough debug info to make the unwinder worker

### Changed

- Improved the error message produced when the unwinder fails

## v0.1.0 - 2020-08-14

Initial release

[Unreleased]: https://github.com/knurling-rs/probe-run/compare/v0.1.3...main
[v0.1.3]: https://github.com/knurling-rs/probe-run/compare/v0.1.2...v0.1.3
[v0.1.2]: https://github.com/knurling-rs/probe-run/compare/v0.1.1...v0.1.2
[v0.1.1]: https://github.com/knurling-rs/probe-run/compare/v0.1.0...v0.1.1
