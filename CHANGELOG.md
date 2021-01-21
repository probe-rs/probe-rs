# Change Log

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/)
and this project adheres to [Semantic Versioning](http://semver.org/).

## [Unreleased]

Note that `probe-run` depends on `defmt-decoder`, `defmt-elf2table` and `defmt-logger`.
The git version of `probe-run` may pull in breaking `defmt` changes, as the git version of `defmt` is currently on its way towards a `v0.2.0` release, while the crates.io version is at `0.1.x`.

- [#122] bumps `defmt` git dependencies to `dd056e6`
- [#125] bumps `defmt` git dependencies to `c4461eb` which includes the new, breaking format string syntax

[#122]: https://github.com/knurling-rs/probe-run/pull/122
[#125]: https://github.com/knurling-rs/probe-run/pull/125

## [v0.1.9] - 2021-01-21

### Added

- [#126] print a list of probes when multiple probes are found and none was selected
- [#133] removes `supported defmt version: c4461eb1484...` from `-h` / ` --help` output

[#126]: https://github.com/knurling-rs/probe-run/pull/126
[#133]: https://github.com/knurling-rs/probe-run/pull/133

### Fixed

- [#129] reject use of defmt logs and the `--no-flash` flag.
- [#132] Make use of the new defmt-logger crate
- [#134] updates `probe-run`'s `defmt` dependencies in order to make new features accessible

[#129]: https://github.com/knurling-rs/probe-run/pull/129
[#132]: https://github.com/knurling-rs/probe-run/pull/132
[#134]: https://github.com/knurling-rs/probe-run/pull/134

## [v0.1.8] - 2020-12-11

### Added

- [#119] `probe-run` has gained a `--connect-under-reset` command line flag. When used, the probe drives the NRST pin of the microcontroller to put it in reset state before establishing a SWD / JTAG connection with the device.

[#119]: https://github.com/knurling-rs/probe-run/pull/119

### Fixed

- [#117] wait for breakpoint before switching RTT from non-blocking mode to blocking mode.

[#117]: https://github.com/knurling-rs/probe-run/pull/117

## [v0.1.7] - 2020-11-26

### Fixed

- [#114] pin `hidapi` dependency to 1.2.3 to enable macOS builds
- [#112] defmt decode errors are now reported to the user
- [#110] colorize `assert_eq!` output

[#114]: https://github.com/knurling-rs/probe-run/pull/114
[#112]: https://github.com/knurling-rs/probe-run/pull/112
[#110]: https://github.com/knurling-rs/probe-run/pull/110

## [v0.1.6] - 2020-11-23

### Fixed

- [#109] `<exception entry>` is not printed twice in the backtrace when the firmware aborts.

[#109]: https://github.com/knurling-rs/probe-run/pull/109

### Changed

- [#108] `probe-rs` has been bumped to version 0.10. This should fix some ST-LINK bugs and expand device support.

[#108]: https://github.com/knurling-rs/probe-run/pull/108

## [v0.1.5] - 2020-11-20

- [#106] `probe-run` now reports the program size
- [#105] `probe-run`'s `--defmt` flag is now optional. `probe-run` will auto-detect the use of the `defmt` crate so the flag is no longer needed.
- [#259] building the crates.io version of `probe-run` no longer depends on the `git` command line tool (fixed [#256])
- [#264] `probe-run` doesn't panic if log message is not UTF-8

[#106]: https://github.com/knurling-rs/probe-run/pull/106
[#105]: https://github.com/knurling-rs/probe-run/pull/105
[#259]: https://github.com/knurling-rs/defmt/pull/259
[#264]: https://github.com/knurling-rs/defmt/pull/264

## [v0.1.4] - 2020-11-11

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
- [#98] the output of `--version` now includes the supported defmt version

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
[#98]: https://github.com/knurling-rs/probe-run/pull/98

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

[Unreleased]: https://github.com/knurling-rs/probe-run/compare/v0.1.9...main
[v0.1.8]: https://github.com/knurling-rs/probe-run/compare/v0.1.8...v0.1.9
[v0.1.8]: https://github.com/knurling-rs/probe-run/compare/v0.1.7...v0.1.8
[v0.1.7]: https://github.com/knurling-rs/probe-run/compare/v0.1.6...v0.1.7
[v0.1.6]: https://github.com/knurling-rs/probe-run/compare/v0.1.5...v0.1.6
[v0.1.5]: https://github.com/knurling-rs/probe-run/compare/v0.1.4...v0.1.5
[v0.1.4]: https://github.com/knurling-rs/probe-run/compare/v0.1.3...v0.1.4
[v0.1.3]: https://github.com/knurling-rs/probe-run/compare/v0.1.2...v0.1.3
[v0.1.2]: https://github.com/knurling-rs/probe-run/compare/v0.1.1...v0.1.2
[v0.1.1]: https://github.com/knurling-rs/probe-run/compare/v0.1.0...v0.1.1
