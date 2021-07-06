# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added initial multicore support. (#565)
- Added SWDv2 multidrop support for multi-DP chips. (#720)
- Added RP2040 target (Raspberry Pi Pico). (#720)

### Target Support

### Changed
- Enabled the generation of global timestamps for ARM targets on `Session::setup_swv`.

### Fixed
- Detect proper USB HID interface to use for CMSIS-DAP v1 probes. Without this, CMSIS-DAP probes with multiple HID interfaces, e.g. MCUlink, were not working properly on MacOS (#722).

## [0.11.0]

### Added

- Support for the `HNONSEC` bit in memory access. This now allows secure access on chips which support TrustZone (#465).
- Support for RISCV chips which use the System Bus Access method for memory access when debugging (#527).
- Support for double buffering in the flash loader, which increased flashing speed (#107).
- Determine location of debug components by parsing ROM table (#431).
- Support for "flashing" data to RAM in the flash loader (#480).
- Added FTDI C232HM-DDHSL-0 to comaptible USB list for FTDI backend (#485).
- Added `--list-probes` and `-n`option to built-in GDB server binary (#486).
- Added RISCV support to GDB server (#493).
- Added `Session::target()` to access the target of a session (#497).
- Support for target description in the GDB server (#498).
- Support for register write commands in the GDB server (#510).
- Added `get_target_voltage()` function to `DebugProbe`, which can be used to read the target voltage if the probe supports it (#533).
- Added `do_chip_erase` flag to `DownloadOptions`, to allow using chip erase when flashing (#537).
- riscv: Support for memory access using system bus (#527).
- Added a generic `read` function, which can be used for memory access with maximum speed, regardless of access width (#633).
- Added an option to skip erasing the flash before programming (#628).
- Added a new debugger for VS Code, using the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/specification). The debugger can be found in the `probe-rs-debugger` crate (#620).
- Additional datatype support for the debugger, plus easier to read display values (#631)
- Added support for raw DAP register reads and writes, using `RawDpAccess`, `RawApAccess` trait (#669, #689, #700).
- Added support for verify after flashing. (#671).
- Handle inlined functions when getting a stack trace (#678).
- Added 'Statics' (static variables) to the stackframe scopes. These are now visible in VSCode between 'Locals' and 'Registers'. This includes some additional datatypes and DWARF expression evaluation capabilities. (#683)
- Added a function to mass erase all memory. (#672).
- Handle Cortex `LOCKUP` status during debugging (#707)

### Target Support

- Added EEPROM region flashing support for STM32L071KBTx (#589).
- Added support for Microchip/Atmel SAM4 (#590).
- Added support for Microchip SAME5x and SAME70 (#596).
- Added support for Microchip SAMD10 (#597).
- Added support for Microchip SAMD11 (#444).
- Fixed support for STM32WB55 (#466).
- Updated target description for LPC55S69 to newest version (#481).
- Use pyocd flash algorithm for NRF52 (#492).
- Added support for flashing NRF52 UICR (#500).
- Updated target description for SAMD21 (#542).
- Support flashes bigger than 128 kBytes on STM32l4xx (#547).
- Added support for LPC546xx (#560).
- Added support for SiLabs EFR32 targets (#566, #567).
- Added support for flashing Intel hex files using `probe-rs-cli` (#618).
- Updated target description for NRF91 (#619).
- Added a RAM benchmark script (#514).
- Initial support for batched commands for J-Link (#515).
- Added support for the STM32F2 family (#675).
- Added support for FE310-G002 (HiFive1 Rev. B).

### Changed

- Renamed `MemoryRegion::Flash` to `MemoryRegion::Nvm` (#482).
- Renamed `FlashInfo` to `NvmInfo`
- Renamed `FlashRegion` to `NvmRegion` and its `flash_info()` method to `nvm_info()`
- Renamed `FlashError::NoSuitableFlash` to `FlashError::NoSuitableNvm`
- The `into_arm_interface` and `into_riscv_interface` functions are replaced by the `try_into_arm_interface` and
  `try_into_riscv_interface` functions, which return the `Probe` struct in the case of an error. This improves the
  auto detection process (#524).
- Improved SWD protocol handling for J-Link (#443, #539, #619).
- Improved error handling for batched CMSIS-DAP commands (#445).
- Use sticky overrun behaviour for improved J-Link performance (#450).
- Better error handling for flashing (#451).
- gdb-server: Halt the chip when attaching (#461).
- Better error messages in the ram_download example (#464).
- Cache value of CSW register to reduce number of SWD transfers (#471).
- Use `erased_byte_value` from target description as default value in the flash loader (#475).
- Added retry functionality for CMSIS-DAP probes (#462).
- riscv: Use abstract commands for CSR access for improved speed (#487).
- The `download_file` and `download_file_with_options` functions now  accept `AsRef<Path>` instead of `&Path`to be more convenient to use (#545, #579).
- Use `itm-decode` to decode ITM packets instead of built-in decoder (#564).
- Flash API Improvements: Data is now owned by the `FlashLoader`and `FlashBuilder` structs to simply the API, and the `FlashLoader::commit()` accepts the `DownloadOptions` struct instead of bool flags (#605).
- Improve internal tracking of core status (#629).
- Rework SWD sequence in J-Link (#513).
- Print ST-Link version in name (#516).
- Improve argument parsing in debugger, add speed option to probe-rs-cli (#523).
- `probe_rs::flashing::DownloadOptions` is now marked `non_exhaustive`, to make it easier to add additional flags in the future.
- Replace `lazy_static` with `once_cell::sync::Lazy` (#685).
- Use new `SendError` instead of `anyhow::Error` in `cmsisdap` module (#687).
  
### Fixed

- Fixed `M33` breakpoints (#543).
- Fixed a bug where ST-Link v3 is not able to read 8 bit data chunks with more than 255 bytes. Currently we set the chunking to 128 bytes. This might be a bug in the ST-Link v3 firmware and might change in the future (#553, #609).
- Errors occuring while trying to open J-Link probes do not prevent other probes from working anymore (#401).
- CMSIS-DAPv1 probes with a HID report size different than 64 bytes are now supported (fixes #282).
- CMSIS-DAPv2 devices are now drained when attaching (fixes #424).
- Improved SWO speed on CMSIS-DAPv2 (fix #448).
- Session auto attach does no longer panic when no probes are connected (#442).
- probe-rs-cli: Halt core before printing backtrace (#447).
- gdb-server: Ensure registers are only read when core is halted (#455).
- Fixed loading Hex files using the flash loader (#472).
- Fixed off-by-one errors when flashing chip with contiguous memory ranges (#574).
- Ensure only ELF segments with type `PT_LOAD` are flashed (#582).
- Fixed overflow in hex file loading, and ensure addresses are calculated correctly (#604).
- riscv: Fixed scanning for harts (#610).
- riscv: Fixed abstract command handling (#611).
- Fixed a bus congestion issue where the chip is polled too often, leading to problems while flashing (#613).
- The breakpoint address is now verified to ensure a breakpoint at the given address is actually possible (#626).
- riscv: Use correct address for access to `abstractauto`register (#511).
- The `--chip` argument now works without specifying the `--elf` argument (fix #517).
- Fixed: Invalid "Unable to set hardware breakpoint", by removing breakpoint caching, instead querying core directly (#632)
- Fix crash on unknown AP class. (#662).
- Fix too many chip erases in chips with multiple NvmRegions. (#670).
- Added missing `skip_erase` setter function introduced in #677 (#679).
- Fixed incorrect array size calculation  (#683)
- STLink: Removed unnecessary SELECT bank switching  (#692)
- STLink: chunk writes in `write_8` to avoid hitting limit (#697)

## [0.10.1]

### Fixed

- Replace calls to `unwrap()` in adi_v5_memory_interface.rs with proper error types (#440).
- Correct URL for Sentry logging in probe-rs-cli-util (#439).

## [0.10.0]

### Added

- Added support for the dedicated ST-Link API which doubles flash write speeds for ST-Link v2 (#369, #377, #397, #435).
- Added support for the STM32WLE.
- Added support for the ATSAMD21 & ATSAMD51.
- Added support for the STM32L1.
- Added support for the EFM32PG12.
- Added support for the MAX32665 & MAX32666.
- Building probe-rs now works without rustfmt being present too (#423).
- Added support for implicit ebreak in RISCV chips (#423, #430).

### Changed

- nRF devices now use the `SoftDevice Erase` algorithm for flashing which will also erase the flash if it contains the softdevice. The previous algorithm prevented users from flashing at all if a softdevice was present (#365, #366).
- The names of probe interface methods were named more consistently (#375).
- FTDI support is now opt in. Please use the `ftdi` feature for support (#378).

### Fixed

- ST-Links now retry the command if a wait was returned in during the SWD transmission (#370).
- Fixed a bug where CMSIS-DAP would not be able to open a probe with a specific VID/PID but no SN specified (#387).
- Fixed a bug where a CMSIS-DAP probe could not be opened if an USB descriptor did not contain any language. This was dominant on macOS (#389).
- Fixed support for the nRF91 (#403).
- Fixed a bug on Windows where paths were not canonicalized properly (#416).
- Fixed a bug where a target fault during AP scans would not be cleared and result in failure on some cores even tho there was no actual issue other than the scan being aborted due to an AP not being present (which is perfectly okay) (#419).
- Use the correct bit mask for the breakpoint comperator on Cortex-M0(+) devices (#434).
- Fixed a bug where breakpoints on M0 would always match the full word even if half word would have been correct (#368).

### Known issues

- Flashing on some chips (known are SAMDx and rare STM32s) with the JLink or CMSIS-DAP probes can be slow. If you see an error involving th DRW or CSW registers, please try using a speed of 100kHz and file a report in #433.

## [0.9.0]

### Added

- Added initial support for FTDI based probes.
- Added support for the STM32L5 family.
- Added support for the STM32G4 family.
- Added support for ITM tracing over SWO in general and drivers for all probes.
- The status LED on CMSIS-DAP probes is now used by probe-rs.

### Changed

- Renamed `ProgressEvent::StartFlashing` to `ProgressEvent::StartProgramming` and `ProgressEvent::PageFlashed` to `ProgressEvent::PageProgrammed` to make naming of events more consistent.

### Fixed

- Fixed a bug where a J-Link would only be opened if the VID, PID AND Serial No. would match. As the Serial is optional, only VID/PID have to match now.
- Fixed a bug with the readout of the serial string that could fail for DAP devices and lead to weird behavior.
- Fixed a bug where the serial number was not printed correctly for some ST-Links.

## [0.8.0]

### Added

- Added support for new devices in the nRF52 family - nRF52805, nRF52820 and nRF52833.
- Added support for the STM32F7 family.
- The `Session` struct and dependants now implement `Debug`.
- The J-Link driver now logs a warning if no proper target voltage is measured.
- The J-Link driver now logs some more information about the connected probe on the `INFO` and `DEBUG` levels.

### Changed

- Improved error handling by a great deal. Errors now can be unwound properly and thus displayed nicely in UI tooling.
- `Core::halt()` now requires a timeout to be specified. This ensures that procedures such as flashing wont time out when certain tasks (like erasing a sector) take longer.

### Fixed

- Fixed a bug where a probe-selector would not work for the JLink if only VID & PID were specified but no serial number.
- Fixed a bug where chip descriptions would fail to parse because of a changed behavior in a newer version of serde_yaml.
- Fixed the LPC55S66 and LPS55S69 targets.
- CMSIS-DAPv1 read operations now properly time out instead of blocking forever, thus giving the user proper feedback.
- Even if an ST-Link cannot be opened (for example on Windows due to a missing driver) it will now be listed properly, just without a serial number.
- Fixed a bug where the J-Link would not be selected properly if no serial number was provided in the selector even if there was a VID:PID pair that matched.

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

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/0.11.0...master
[0.11.0]: https://github.com/probe-rs/probe-rs/compare/v0.10.1...0.11.0
[0.11.0-alpha.1]: https://github.com/probe-rs/probe-rs/compare/v0.10.1...0.11.0-alpha.1
[0.10.1]: https://github.com/probe-rs/probe-rs/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/probe-rs/probe-rs/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/probe-rs/probe-rs/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/probe-rs/probe-rs/compare/v0.7.1...v0.8.0
[0.7.1]: https://github.com/probe-rs/probe-rs/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/probe-rs/probe-rs/compare/v0.6.2...v0.7.0
[0.6.2]: https://github.com/probe-rs/probe-rs/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/probe-rs/probe-rs/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/probe-rs/probe-rs/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/probe-rs/probe-rs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/probe-rs/probe-rs/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/probe-rs/probe-rs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/probe-rs/probe-rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.2.0
