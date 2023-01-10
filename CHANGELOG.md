# Change Log

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/) and this project adheres to [Semantic Versioning](http://semver.org/).

## [Unreleased]

- [#371] Add "missing drivers" to troubleshooting section
- [#366] Update CI

[#371]: https://github.com/knurling-rs/probe-run/pull/371
[#366]: https://github.com/knurling-rs/probe-run/pull/366

## [v0.3.5] - 2022-10-07

- [#357] Update to `clap 4.0`
- [#349] Add `PROBE_RUN_SPEED` env variable
- [#345] Update dev-dependency `serial_test`
- [#344] Replace `pub(crate)` with `pub`
- [#343] Mark `v0.3.4` as released in `CHANGELOG.md`
- [#353] Add `--verify` option to verify written flash

[#357]: https://github.com/knurling-rs/probe-run/pull/357
[#349]: https://github.com/knurling-rs/probe-run/pull/349
[#345]: https://github.com/knurling-rs/probe-run/pull/345
[#344]: https://github.com/knurling-rs/probe-run/pull/344
[#343]: https://github.com/knurling-rs/probe-run/pull/343

## [v0.3.4] - 2022-08-10

- [#342] Release `probe-run 0.3.4`
- [#339] Simplify `fn round_up`
- [#337] Clean snapshot tests
- [#335] Add unit tests for `fn round_up`
- [#334] Simplify snapshot tests
- [#333] Clean up `enum Outcome`
- [#331] Refactor stack painting
- [#330] Fix `fn round_up`
- [#329] Update probe-rs to 0.13.0 (does not yet implement 64-bit support)
- [#328] Simplify, by capturing identifiers in logging macros
- [#327] Optimize stack usage measuring
- [#326] Make use of i/o locking being static since rust `1.61`.
- [#321] CI: Add changelog-enforcer
- [#320] Disable terminal colorization if `TERM=dumb` is set
- [#319] Warn on target chip mismatch
- [#317] Clarify "can't determine stack overflow" error message
- [#314] Clarify documentation in README
- [#305] Add option to disable double buffering while loading firmware
- [#293] Update snapshot tests to new TRACE output

[#342]: https://github.com/knurling-rs/probe-run/pull/342
[#339]: https://github.com/knurling-rs/probe-run/pull/339
[#337]: https://github.com/knurling-rs/probe-run/pull/337
[#335]: https://github.com/knurling-rs/probe-run/pull/335
[#334]: https://github.com/knurling-rs/probe-run/pull/334
[#333]: https://github.com/knurling-rs/probe-run/pull/333
[#331]: https://github.com/knurling-rs/probe-run/pull/331
[#330]: https://github.com/knurling-rs/probe-run/pull/330
[#329]: https://github.com/knurling-rs/probe-run/pull/329
[#328]: https://github.com/knurling-rs/probe-run/pull/328
[#327]: https://github.com/knurling-rs/probe-run/pull/327
[#326]: https://github.com/knurling-rs/probe-run/pull/326
[#321]: https://github.com/knurling-rs/probe-run/pull/321
[#320]: https://github.com/knurling-rs/probe-run/pull/320
[#319]: https://github.com/knurling-rs/probe-run/pull/319
[#317]: https://github.com/knurling-rs/probe-run/pull/317
[#314]: https://github.com/knurling-rs/probe-run/pull/314
[#305]: https://github.com/knurling-rs/probe-run/pull/305
[#293]: https://github.com/knurling-rs/probe-run/pull/293

## [v0.3.3] - 2022-03-14

### Fixed

- [#311] fixed "probe-run does not work with some programs that have less than 10 KiB of stack space _unless_ `--measure-stack` is available"

[#311]: https://github.com/knurling-rs/probe-run/pull/311

## [v0.3.2] - 2022-03-10 - YANKED

- [#303] Don't bail, but only warn if using `--no-flash` with `defmt`
- [#302] Make stack painting fast again! 🇪🇺
- [#301] Add way to pass chip description file
- [#300] Simplify verbose matching
- [#299] Fix and refactor `fn extract_stack_info`
- [#296] turn some `println!` into `writeln!`
- [#295] probe-run json output
- [#294] Update `Cargo.lock`

[#303]: https://github.com/knurling-rs/probe-run/pull/303
[#302]: https://github.com/knurling-rs/probe-run/pull/302
[#301]: https://github.com/knurling-rs/probe-run/pull/301
[#300]: https://github.com/knurling-rs/probe-run/pull/300
[#299]: https://github.com/knurling-rs/probe-run/pull/299
[#296]: https://github.com/knurling-rs/probe-run/pull/296
[#295]: https://github.com/knurling-rs/probe-run/pull/295
[#294]: https://github.com/knurling-rs/probe-run/pull/294

## [v0.3.1] - 2021-11-26

- [#287]: unwind: skip FDEs with initial address of 0
- [#286]: Update dependencies
- [#285]: Update `probe-rs` and `probe-rs-rtt` to `0.12`
- [#282]: Include program counter value in backtrace when -v is passed
- [#281]: Report flashing size using probe-rs's FlashProgress system
- [#280]: Turn symbol demangling back on

[#287]: https://github.com/knurling-rs/probe-run/pull/287
[#286]: https://github.com/knurling-rs/probe-run/pull/286
[#285]: https://github.com/knurling-rs/probe-run/pull/285
[#282]: https://github.com/knurling-rs/probe-run/pull/282
[#281]: https://github.com/knurling-rs/probe-run/pull/281
[#280]: https://github.com/knurling-rs/probe-run/pull/280

## [v0.3.0] - 2021-11-09

- [#273]: Update to Rust 2021 🎉
- [#267]: Minimize dependencies by disabling default-features
- [#266]: Recover from decoding-errors
- [#247]: Print troubleshooting information on "probe not found" error
- [#264]: Use new stream decoder API
- [#263]: Set blocking mode to `0b10` (`BLOCK_IF_FULL`)
- [#257]: Split "set rtt to blocking-mode" into function
- [#259]: Update snapshot tests
- [#260]: Fix CI on nightly
- [#254]: Add support for measuring the program's stack usage
- [#248]: Print dedicated message on control + c
- [#250]: Backtrace options
- [#253]: feat: add help message for JtagNoDeviceConnected
- [#251]: Removes call to fill in user survey from readme.
- [#246]: Enable deactivated tests for Windows

[#273]: https://github.com/knurling-rs/probe-run/pull/273
[#267]: https://github.com/knurling-rs/probe-run/pull/267
[#266]: https://github.com/knurling-rs/probe-run/pull/266
[#247]: https://github.com/knurling-rs/probe-run/pull/247
[#264]: https://github.com/knurling-rs/probe-run/pull/264
[#263]: https://github.com/knurling-rs/probe-run/pull/263
[#257]: https://github.com/knurling-rs/probe-run/pull/257
[#259]: https://github.com/knurling-rs/probe-run/pull/259
[#260]: https://github.com/knurling-rs/probe-run/pull/260
[#254]: https://github.com/knurling-rs/probe-run/pull/254
[#248]: https://github.com/knurling-rs/probe-run/pull/248
[#250]: https://github.com/knurling-rs/probe-run/pull/250
[#253]: https://github.com/knurling-rs/probe-run/pull/253
[#251]: https://github.com/knurling-rs/probe-run/pull/251
[#246]: https://github.com/knurling-rs/probe-run/pull/246

## [v0.2.5] - 2021-08-02

- [#244] Fix clippy warnings
- [#240] Link Knurling User Survey in `README`
- [#235] Update to `probe-rs` `0.11`
- [#234] Feature-gate windows tests
- [#233] Some clarifications and corrections

[#244]: https://github.com/knurling-rs/probe-run/pull/244
[#240]: https://github.com/knurling-rs/probe-run/pull/240
[#235]: https://github.com/knurling-rs/probe-run/pull/235
[#234]: https://github.com/knurling-rs/probe-run/pull/234
[#233]: https://github.com/knurling-rs/probe-run/pull/233

## [v0.2.4] - 2021-06-17

- [#212] make `unwind::target()` infallible
- [#216] Fix `EXC_RETURN` detection on thumbv8
- [#218] add first, user-triggered snapshot tests
- [#219] add more explicit hint if elf path doesn't lead to an existing file
- [#221] Obtain git-version from macro, instead of custom build-script
- [#222] refactor the huge "main" function into smaller functions + modules
- [#224] target_info: print ram region again
- [#225] `cli::tests`: rstest-ify tests for `fn extract_git_hash`
- [#226] `CI`: Run tests and clippy
- [#228] Remove unused file `utils.rs`

[#212]: https://github.com/knurling-rs/probe-run/pull/212
[#216]: https://github.com/knurling-rs/probe-run/pull/216
[#218]: https://github.com/knurling-rs/probe-run/pull/218
[#219]: https://github.com/knurling-rs/probe-run/pull/219
[#221]: https://github.com/knurling-rs/probe-run/pull/221
[#222]: https://github.com/knurling-rs/probe-run/pull/222
[#224]: https://github.com/knurling-rs/probe-run/pull/224
[#225]: https://github.com/knurling-rs/probe-run/pull/225
[#226]: https://github.com/knurling-rs/probe-run/pull/226
[#228]: https://github.com/knurling-rs/probe-run/pull/228

## [v0.2.3] - 2021-05-21

### Improvements
- [#193] Check `PROBE_RUN_IGNORE_VERSION` on runtime
- [#199] Add column info to backtrace
- [#200] Highlight frames that point to local code in backtrace
- [#203] + [#209] + [#210] Add `--shorten-paths`
- [#204] Make 'stopped due to signal' force a backtrace
- [#207] Read as little stacked registers as possible during unwinding

### Docs
- [#190] `README`: Replace ${PROBE_RUN_CHIP} in code example
- [#192] + [#194] `README`: Add installation instructions for Fedora and Ubuntu

### Fixes
- [#206] Fix unwinding exceptions that push FPU registers onto the stack

### Internal improvements
- [#197] Refactor "print backtrace" code
- [#211] `mv backtrace.rs backtrace/mod.rs`

[#193]: https://github.com/knurling-rs/probe-run/pull/193
[#199]: https://github.com/knurling-rs/probe-run/pull/199
[#200]: https://github.com/knurling-rs/probe-run/pull/200
[#203]: https://github.com/knurling-rs/probe-run/pull/203
[#204]: https://github.com/knurling-rs/probe-run/pull/204
[#207]: https://github.com/knurling-rs/probe-run/pull/207
[#190]: https://github.com/knurling-rs/probe-run/pull/190
[#192]: https://github.com/knurling-rs/probe-run/pull/192
[#194]: https://github.com/knurling-rs/probe-run/pull/194
[#206]: https://github.com/knurling-rs/probe-run/pull/206
[#209]: https://github.com/knurling-rs/probe-run/pull/209
[#210]: https://github.com/knurling-rs/probe-run/pull/210
[#197]: https://github.com/knurling-rs/probe-run/pull/197
[#211]: https://github.com/knurling-rs/probe-run/pull/211

## [v0.2.2] - 2021-05-06

### Improvements

- [#163] Report exit reason, make `backtrace` optional
- [#171] Introduce even more verbose log level
- [#174] Let developers skip defmt version check
- [#179] Limit `backtrace` length, make limit configurable
- [#184] Add some bounds checking to `unwinding`

#### Docs

- [#161] Remind the user that the `bench` profile should be also overridden
- [#181] `README`: add copypasteable example how to run from repo
- [#183] `README`: Add troubleshooting for use with RTIC

### Fixes

- [#162] Remove `panic-probe`

### Internal Improvements

- [#165] Various simplifications
- [#175] Run `cargo fmt -- --check` in CI

[#161]: https://github.com/knurling-rs/probe-run/pull/161
[#162]: https://github.com/knurling-rs/probe-run/pull/162
[#163]: https://github.com/knurling-rs/probe-run/pull/163
[#165]: https://github.com/knurling-rs/probe-run/pull/165
[#171]: https://github.com/knurling-rs/probe-run/pull/171
[#174]: https://github.com/knurling-rs/probe-run/pull/174
[#175]: https://github.com/knurling-rs/probe-run/pull/175
[#179]: https://github.com/knurling-rs/probe-run/pull/179
[#181]: https://github.com/knurling-rs/probe-run/pull/181
[#184]: https://github.com/knurling-rs/probe-run/pull/184
[#183]: https://github.com/knurling-rs/probe-run/pull/183

## [v0.2.1] - 2021-02-23

- [#158] Fix Ctrl+C handling

[#158]: https://github.com/knurling-rs/probe-run/pull/158

## [v0.2.0] - 2021-02-22

### New Features

- [#153] Update to defmt 0.2.0
- [#152] Allow selecting a probe by serial number
- [#149] Update and deduplicate dependencies

### Fixes

- [#141] Address Clippy lints

[#141]: https://github.com/knurling-rs/probe-run/pull/141
[#149]: https://github.com/knurling-rs/probe-run/pull/149
[#152]: https://github.com/knurling-rs/probe-run/pull/152
[#153]: https://github.com/knurling-rs/probe-run/pull/153

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

[Unreleased]: https://github.com/knurling-rs/probe-run/compare/v0.3.5...main
[v0.3.4]: https://github.com/knurling-rs/probe-run/compare/v0.3.4...v0.3.5
[v0.3.4]: https://github.com/knurling-rs/probe-run/compare/v0.3.3...v0.3.4
[v0.3.3]: https://github.com/knurling-rs/probe-run/compare/v0.3.2...v0.3.3
[v0.3.2]: https://github.com/knurling-rs/probe-run/compare/v0.3.1...v0.3.2
[v0.3.1]: https://github.com/knurling-rs/probe-run/compare/v0.3.0...v0.3.1
[v0.3.0]: https://github.com/knurling-rs/probe-run/compare/v0.2.5...v0.3.0
[v0.2.5]: https://github.com/knurling-rs/probe-run/compare/v0.2.4...v0.2.5
[v0.2.4]: https://github.com/knurling-rs/probe-run/compare/v0.2.3...v0.2.4
[v0.2.3]: https://github.com/knurling-rs/probe-run/compare/v0.2.2...v0.2.3
[v0.2.2]: https://github.com/knurling-rs/probe-run/compare/v0.2.1...v0.2.2
[v0.2.1]: https://github.com/knurling-rs/probe-run/compare/v0.2.0...v0.2.1
[v0.2.0]: https://github.com/knurling-rs/probe-run/compare/v0.1.9...v0.2.0
[v0.1.9]: https://github.com/knurling-rs/probe-run/compare/v0.1.8...v0.1.9
[v0.1.8]: https://github.com/knurling-rs/probe-run/compare/v0.1.7...v0.1.8
[v0.1.7]: https://github.com/knurling-rs/probe-run/compare/v0.1.6...v0.1.7
[v0.1.6]: https://github.com/knurling-rs/probe-run/compare/v0.1.5...v0.1.6
[v0.1.5]: https://github.com/knurling-rs/probe-run/compare/v0.1.4...v0.1.5
[v0.1.4]: https://github.com/knurling-rs/probe-run/compare/v0.1.3...v0.1.4
[v0.1.3]: https://github.com/knurling-rs/probe-run/compare/v0.1.2...v0.1.3
[v0.1.2]: https://github.com/knurling-rs/probe-run/compare/v0.1.1...v0.1.2
[v0.1.1]: https://github.com/knurling-rs/probe-run/compare/v0.1.0...v0.1.1
