# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added support for new devices in the nRF52 family. nRF52805, nRF52820 and nRF52833.

### Changed

### Fixed

## [0.7.1]

### Changed

- `DebugProbeType` is now public.
- Update LPC55S66/LPC55S69 targets.

### Fixed

- Add missing core value for LPC55S66 and LPC55S69.

## [0.7.0]

### Added

- Added support for RISC-V flashloaders! An example how to write one can be found here: https://github.com/Tiwalun/hifive-flashloader.
- Added support for LLDB (works better than GDB in most cases; try it!).
- Added support for specifying a probe via VID, PID and serial number.

### Changed

- The probe-rs API was changed that no internal `Rc<RefCell<T>>`s are present anymore to enable multithreading and make the API cleaner (see https://github.com/probe-rs/probe-rs/pull/240 for the changes).
- Cleaned up the gernal GDB server code.
- Make some parts of the API public such that custom APs can be implemented and used for ARM targets (see https://github.com/probe-rs/probe-rs/pull/249, https://github.com/probe-rs/probe-rs/pull/253)
- Removed a great deal of (non-panicking) unwraps inside the code.
- Improved erroring by a great deal. Removed error stacking and started using anyhow for upper-level errors. This allows for nicer error printing!

### Fixed

- Fixed a bug where an empty DAP-Link batch would just crash without a proper error message.
- Fixed a check where the serial number of the stlink which would be supported at a minimum was too low (off by one).
- Fixed the broken vCont & memory-map commands in the GDB stub.
- Fixed deserialization of flash algorithm descriptions which enables to load target descriptions during runtime.
- Fixed an issue where the error message would say that more than one probe was found when no probe was detected at all.
- Fixed a bug in the gdb-server that causes it to never halt after a continue.
- Fixed an issue where the gdb-server would always use 100 % cpu time of the core it's running on.

## [0.6.2]

### Added

- `WireProtocol` now implements `Serialize`.

### Fixed

- The GDB stub will no longer crash when GDB tries to access invalid memory.

### Known issues

- Some ST M3s such as the STM32F103 are known to have reset issues. See [#216](https://github.com/probe-rs/probe-rs/pull/216).

## [0.6.1]

### Added

- Support for the STM32F3 family was added.
- Added support for most Holtek ARM chips.
- Added support for the STM32H7 and M7 cores.

### Changed

- DAPlink implementation now batches `read_register` and `write_register`
  commands, executing the entire batch when either the batch is full or a
  `read_register` is requested, returning the read result or an error which
  may indicate an error with a batched command. As a consequence,
  `write_register` calls may return `Ok(())` even if they have not been
  submitted to the probe yet, but any read will immediately execute the batch.
  Operations such as device flashing see around 350% speedup.
- Improved error handling for STLinks that have an older firmware which doesn't support multiple APs.
- The flash layout reporting struct is less verbose now.

### Fixed

- Fix a bug in the CLI where it would always be unable to attach to the probe.

### Known issues

- Some ST M3s such as the STM32F103 are known to have reset issues. See [#216](https://github.com/probe-rs/probe-rs/pull/216).

## [0.6.0]

### Added

- Flashing support for the STM32L4 series.
- Added the possibility to set the speed on DebugProbes and also implemented it for all three supported probes (CMSIS-DAP, ST-Link and J-Link).
- Make M3 cores selectable from built in targets.
- Make the filling of erased flash sectors with old contents possible. When flashing, the minimal erase unit is a sector. If the written contents do not span a sector, we would erase portions of the flash which are not written afterwards. Sometimes that is undesired and one wants to only replace relevant parts of the flash. Now the user can select whether they want to restore unwritten but erased parts to the previous contents. The flash builder now automatically reads to be erased and not written contents beforehand and adds them to the to be written contents.
- Added a flash visualizer which can generate an SVG of the layouted flash contents.

### Changed

- Improved error handling for the flash download module.
- Improved error messages for ARM register operations.
- The `flash` module has been renamed to `flashing`.
- Downloading a file now has the possibility to add options instead of multiple parameters to clean up the interface.
- `read8`/`write8` implement true 8-bit accesses if they are supported by target.
- Improved build times by changing code generation for targets. For more details, see [PR #191](https://github.com/probe-rs/probe-rs/pull/191).
- Improved logging for ELF loading. If there was no loadable sections before, nothing would happen. Now it is properly reported, that there was no loadable sections.

### Fixed

- Fix the usage of ST-Link V3.
- Removed an unwrap that could actually crash.
- Fixed a bug where reading a chip definition from a YAML file would always fail because parsing a `ChipFamily` from YAML was broken.
- Fixed a bug in the ST-Link support, where some writes were not completed. This lead to problems when flashing a device, as the
  final reset request was not properly executed.
- Refactored 8-bit memory access in ADIMemoryInterface, fixing some edge case crashes in the process. Also rewrote all tests to be more thorough.
- Fixed 8/16-bit memory access processing in `MockMemoryAP`.
- Protocol selection for JLink now will properly honor the actual capabilities of the JLink instead of crashing if the capability was missing.
- Fix an issue where probes would double attach to a target, potentially leading to issues.

## [0.5.1]

### Fixed

- Fix a bug where M3 targets would not be able to load the core.

## [0.5.0]

### Added

- Flashing support for the STM32G0 series.
- Flashing support for the STM32F0 series.
- Flashing support for the STM32WB55 series.
- Support for RISCV debugging using a Jlink debug probe.
- Support for SWD debugging using a Jlink debug probe.

### Changed

- The entire API was overhauled. The Probe, Session and Core structs have different interaction and APIs now.
  Please have a look at the docs and examples to get an idea of the new interface.
  The new API supports multiple architectures and makes the initialization process until the point where you can talk to a core easier.
  The core methods don't need a passed probe anymore. Instead it stores an Rc to the Session object internally. The Probe object is taken by the Session which then can attach to multiple cores.
  The modules have been cleaned up. Some heavily nested hierarchy has been flattened.
- More consistent and clean naming and reporting of errors in the stlink and daplink modules. Also the errorhandling for the probe has been improved.

### Fixed

- Various fixes

### Known issues

- Some chips do not reset automatically after flashing
- The STM32L0 cores have issues with flashing.

## [0.4.0]

### Added

- A basic GDB server was added \o/ You can either use the provided `gdb-server` binary or use `cargo flash --gdb` to first flash the target and then open a GDB session. There is many more new options which you can list with `cargo flash --help`.
- Support for multiple breakpoints was added. Breakpoints can now conveniently be set and unset. probe-rs checks for you that there is a free breakpoint and complains if not.
- A flag to disable progressbars was added. Error reporting was broken because of progressbar overdraw. Now one can disable progress bars to see errors. In the long run this has to be fixed.
- Added an improved way to create a `Probe`.
- Added an older USB PID to have probe-rs detect older STLinks with updated Firmware.
- Added support for flashing with different sector properties. This fixed broken flashing on the STM M4s.

### Changed

- Code generation for built in targets was split off into a separate crate so probe-rs can be built without built in targets if one doesn't want them.

### Fixed
- Fixed setting and clearing breakpoints on M4 cores.

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

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.7.1...master
[0.7.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.7.0...v0.7.1
[0.7.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.2...v0.7.0
[0.6.2]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.1...v0.6.2
[0.6.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.0...v0.6.1
[0.6.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.5.1...v0.6.0
[0.5.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.5.0...v0.5.1
[0.5.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.4.0...v0.5.0
[0.4.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.3.0...v0.4.0
[0.3.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0...v0.3.0
[0.2.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0
