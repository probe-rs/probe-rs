# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.2]

Released 2023-01-18

- Added STM32С0 target (STM32С011 and STM32С031). (#1403)

### Fixed

- stlink: fix retries on DP/AP WAIT errors. (#1406)

## [0.14.1]

Released 2023-01-14

## [0.14.0]

Released 2023-01-13

### Added

- Added PartialEq Trait to the struct DebugProbeInfo. (#1173)
- Added support for configuring trace data destinations (#1177)
  - Tracing on M4 architectures utilize the TPIU for all hardware tracing (#1182)
- ITM tracing can now be completed using the probe-rs CLI (#1180)
- Added support for MIMXRT10xx targets (#1174)
- Added support for the Cortex M7 of MIMXRT11xx targets (#1250)
- Added support for in-line (column specific) breakpoints where multiple statements (potential breakpoints) are on the same line of source code. (#1156)
- Added support for MSP432P4XX targets (#1201)
- Added support for Microchip SAMDA1
- Added Probe re-attach handling when needed after `debug_device_unlock`
- Added Custom ArmDebugSequence for ATSAM D5x/E5x devices
- Added a `FlashLoader::data` method (#1254)
- Added Support for STM32H735 family. (#913)
- Added support for MAX32660 target (#1249)
- Added support for W7500 target
- Added an optional `stack_size` configuration to flash algorithms to control the stack size (#1260)
- Added Support for Debug Erase Sequences that (if available) are used instead of the normal chip-erase logic
- Added Support for GD32E50x targets (#1304)
- Added support for the Infineon XMC4000 family
- Added support for the Infineon XMC4000 family (#1301)
- Added debug support for viewing function arguments (#1333)
- Added support for the EFM32GG11B family (#1346)
- Added support for finding targets externally (#1338)

### Changed

- SWV vendor configuration has been refactored into sequences and trace functions have been renamed:
  - `Session::setup_swv` has been renamed to `Session::setup_tracing`
  - `Session::read_swo` has been renamed to `Session::read_trace_data`
- `probe-rs-debugger`: RISC-V `ebreak` instruction will enter Debug Mode (#1213)
- RTT: When a channel format is `defmt`, automatically set the channel mode to `BlockingIfFull` on attach. (Enhancement request #1161)
- RTT: Report data decode errors when channel format is `defmt`. (#1243)
  - Note: This is a breaking API change for `probe_rs_cli::rtt::RttActiveChannel::get_rtt_data()`. To mitigate the impact of this change:
    - `probe_rs_cli::rtt::RttActiveTarget::poll_rtt()` will maintain the original signature and behaviour of ignoring errors from `defmt` until deprecated in 0.14.0.
    - The new `probe_rs_cli::rtt::RttActiveTarget::poll_rtt_fallible()` will propagate errors from `get_rtt_data()` on any of the active channels.
- target-gen: Various changes and optimizations: (#1259)
  - Memory addresses and sizes in YAML are generated in hex format, for improved readability.
  - Remove `Option::is_none`, empty `Vec`, and `false` bool values, in generated YAML, for improved readability.
  - Generate all pack file specified memory regions.
  - Match memory regions to pack file specified core names.
- `probe_rs_target::chip::Chip` has a new field `pack_file_release` which is populated by `target-gen`.(#1259)
- Benchmarking code moved from an example to `probe-rs-cli` subcommand (#1296).
- Replace `log` crate, with `tracing` in `probe-rs-debugger` executable, and in the `rtt` library. (#1297)
- Improved formatting of `probe-rs-cli info` output. (#1305)
- Refactor VSCode handling of logging and user messaging - see [VSCode PR #37](https://github.com/probe-rs/vscode/pull/37) (#1334)
- Refactor error handling, split `crate::Error::ArchitectureSpecific` into two separate variants for RISC-V and ARM, and create a new `ArmError` enum for ARM specific errors. (#1344)

### Fixed

- (#1351) Warning messages about duplicate packages when using `probe-rs` as a library
- (#1269) Error message in case of FTDI device access issues.
- (#350) Flashing and debugging on STM32 chips using WFI instructions should now be stable (fixed in #1177)
- Fixed rtthost --scan-region to properly support memory range scannig. (#1192)
- Debug: Improve logic for halt locations used by breakpoints and stepping. (#1156)
- Debug: Some in-scope variables are excluded from stack_trace. (#1156)
- Debug: Ensure RTT buffer on target is reported to DAP client in 'timely' manner. (#1208)
- Debug: Provide unique default names on DAP client, when multiple RTT Channels have no configured name. (#1208)
- Added missing memory regions for ESP32.yaml file, to fix RTT Channel name issue. (#1209)
- Fix maximum addressable Flash size in ESP32.yaml file, to be 16Mb (was 64Mb). (#1209)
- Debug: Enable stepping or running past a BKPT (Arm Cortex-M) or EBREAK (RISC-V) instruction (#1211).
- (#1058) Non-successful DAP Transfer requests no longer require response data to be present.
- Debug: Gracefully handle stack unwind when CFA rule references a FP with value of zero. (#1226)
- Debugger: Improve core status checking during launch.(#1228)
- Debugger: Prevent stack overflows when expanding "static" section in probe-rs-debugger. (#1231)
- RTT: Prevent panicking in `probe-rs-cli-util/src/rtt/rs` when defmt stream decoding provides invalid frame index. (#1236)
- Fix: Attaching to LPC55S69 seems to stop code execution - incorrect values in target YAML. (#1220)
- Debug: Fix `probe-rs-debugger` crashes when variable unwind fails with excessively long error messages. (#1252)
- Fix: Dual core devices had incorrect 'core' names in `STM32H7_Series.yaml`, causing panic during flashing. (#1023)
- Fix: Include all RAM regions in `STM32H7_Series.yaml` (#429)
- Fix: Include all new STM32H7 variants from the latest CMSIS pack file (#913)
- Fix: Update STM32G0_Series.yaml to include latest variants (STM32G050, STM32G051, STM32G061, STM32G0B0, STM32G0B1, STM32G0C1) (#1266)
- Fix: Correct flash algorithm values in LPC55S69.yaml. (#1220)
- Fix: Timeout during flashing when using connect under reset - regression from #1259. (#1286)
- Fix: Validate RiscV CSR addresses to avoid unnecessary panics. (#1291)
- Debugger: Fix unpredictable behaviour when breaking on, or stepping over macros. (#1230)
- Fix: Extend fix for WFI instructions (#1177) to STM32F1
- Debugger: RTT data from target is now polled/reported in a timely manner, during stepping, and after breakpoint halting. (#1341)

## [0.13.0]

### Added

- Added an option to disable use of double-buffering when downloading flash (#1030, #883)
- rtt::ChannelMode implements additional traits: Clone, Copy, serde's Serialize and Deserialize
- Added a permissions system that allows the user to specify if a full chip erase is allowed (#918)
- Added debug sequence for the nRF5340 that turns on the network core can unlock both cores by erasing them if that is permitted (#918)
- Support for core registers `msp`, `psp` and `extra`, extra containing:
  - Bits[31:24] CONTROL.
  - Bits[23:16] FAULTMASK.
  - Bits[15:8] BASEPRI.
  - Bits[7:0] PRIMASK.
- Debug port start sequence for LPC55S16. (#944)
- Added a command to print the list of all supported chips. (#946)
- Added a command to print info about a chip, such as RAM and the number of cores. (#946)
- ARM:`Session::swo_reader` that returns a wrapping implementation of `std::io::Read` around `Session::read_swo`. (#916)
- Added CortexM23 to Armv8m mapping for `target-gen`. (#966)
- Added get_target_voltage to the Probe struct to access the inner DebugProbe method. (#991)
- Debugger: Added support for showing multiple inlined functions in backtrace. (#1002)
- Debugger: Add support LocLists (attribute value of DW_AT_location) (#1025)
- Debugger: Add support for DAP Requests (ReadMemory, WriteMemory, Evaluate & SetVariable) (#1035)
- Debugger: Add support for DAP Requests (Disassemble & SetInstructionBreakpoints) (#1049)
- Debugger: Add support for stepping at 'statement' level, plus 'step in', 'step out' (#1056)
- Debugger: Add support for navigating and monitoring SVD Peripheral Registers. (#1072)
- Added GD32F3x0 series support (#1079)
- Added support for connecting to ARM devices via JTAG to the JLink probe
- Added preliminary support for ARM v7-A cores
- Added preliminary support for ARM v8-A cores
- CLI Debugger: Added 8-bit read / write memory commands
- Added Arm Serial-Wire-View (SWV) support for more targets (e.g. STM32H7 families) (#1117)
  - Support added for trace funnels and SWO peripherals
  - Added custom sequencing for STM32H7 parts to configure debug system components on attach
- Added support for ARMv8-A cores running in 64-bit mode (#1120)
- Added FPU register reading support for cortex-m cores
- Added support for Huada Semiconductor HC32F005 MCUs.
- Added FPU register support for Cortex-A cores (#1154)
- GDB now reports the core name in `info threads` (#1158)
- Added a recover sequence for the nRF9160 (#1169)

### Changed

- ARM reset sequence now retries failed reads of DHCSR, fixes >500kHz SWD for ATSAMD21.
- Chip names are now matched treating an 'x' as a wildcard. (#964)
- GDB server is now available as a subcommand in the probe-rs-cli, not as a separate binary in the `gdb-server` package anymore. (#972)
- `probe_rs::debug` and `probe-rs-debugger` changes/cleanup to the internals (#1013)
  - Removed StackFrameIterator and incorporated its logic into DebugInfo::unwind()
  - StackFrame now has VariableCache entries for locals, statics and registers
  - Modify DebugSession and CoreData to handle multiple cores.
  - Modify Variable::parent_key to be Option<i64> and use None rather than 0 values to control logic.
  - Use the updated StackFrame, and new VariableNodeType to facilitate 'lazy' loading of variables during stack trace operations. VSCode and MS DAP will request one 'level' of variables at a time, and there is no need to resolve and cache variable data unless the user is going to view/use it.
  - Improved `Variable` value formatting for complex variable types.
- Updated STM32H7 series yaml to support newly released chips. (#1011)
- Debugger: Removed the CLI mode, in favour of `probe-rs-cli` which has richer functionality. (#1041)
- Renamed `Probe::speed` to `Probe::speed_khz`.
- Debugger: Changes to DAP Client `launch.json` to prepare for WIP multi-core support. (#1072)
- `ram_download` example now uses clap syntax.
- Refactored `probe-rs/src/debug/mod.rs` into several smaller files. (#1082)
- Update STM32L4 series yaml from Keil.STM32L4xx_DFP.2.5.0. (#1086)
- Debugger: SVD uses new `expand` feature of `svd-parser` crate to expand arrays and clusters. (#1090)
- Updated cmsis-pack dependency to version 0.6.0. (#1089)
- Updated all parameters and fields that refer to memory addresses from u32 to u64 in preparation for 64-bit target support. (#1115)
- Updated `Core::read_core_reg` and `Core::write_core_reg` to work with both 32 and 64-bit values (#1119)
- Renamed `core::CoreRegisterAddress` to `core::RegisterId`, and `core::CoreRegister` to `core::MemoryMappedRegister`. (#1121)
- Updated gdb-server to use gdbstub internally (#1125)
- gdb-server now uses all cores on a target (#1125)
- gdb-server now supports floating point registers (#1133)
- Debug: Correctly handle compressed vs non-compressed instructions sets for RISC-V. (#1224)
- The core now needs to be halted for core register access. (#1044)
- The memory functions to do memory transfers have been standardized. This effectively means that `read_*` and `write_*` do what the name says unconditionally. E.g. `read_8` will always do 8 bit reads or `write_32` will always do 32 bit writes. New functions that are called `read` and `write` have been introduced. Those will try to maximize throughput. They mix transfer sizes however they see fit. If you need to use a feature of a chip that requires a specific transfer size, please resort to the `read_*` and `write_*` functions. (#1078)

### Fixed

- Fixed a panic when cmsisdap probes return more transfers than requested (#922, #923)
- `probe-rs-debugger` Various fixes in PR. (#895)
  - Fix stack overflow when unwinding circular references in data structures. (#894)
  - Reworked the stack unwind in `StackFrameIterator::new()` and `StackFrameIterator::next()`
    - More reliable backtrace and register values for previous frames in the stack.
    - Lazy (on demand) load of &lt;statics&gt; variables to avoid overhead during debugging.
    - More accurate breakpoint handling from VSCode extension.
    - Virtual frames for `inlined` functions, that can step back to the call site.
  - A fix to adapt to Rust 2021 encoding of Dwarf `DW_AT_discr_value` tags for variants.
  - Updated MS DAP Protocol to 1.51.1.
  - Adapt to `defmt` 0.3 'Rzcobs' encoding to fix [VSCode #26](https://github.com/probe-rs/vscode/issues/26).
  - Support the new `defmt` 0.3 `DEFMT_LOG` environment variable.
  - Requires `probe-rs/vscode` [PR #27](https://github.com/probe-rs/vscode/pull/27)
  - Debugger: Improved RTT reliability between debug adapter and VSCode (#1035)
  - Fixed missing `derive` feature for examples using `clap`.
  - Increase SWD wait timeout (#994)
  - Debugger: Fix `Source` breakpoints only worked for a single source file. (#1098)
  - Debugger: Fix assumptions for ARM cores
  - GDB: Fix assumptions for ARM cores
- Fixed access to Arm CoreSight components being completed through the wrong AP (#1114)
- Debug: Additions to complete RISC-V and 64-bit support. (#1129)
  - probe_rs::debug::Registers uses new `core::RegisterId` and `core::RegisterValue` for consistent register handling.
  - RISC-V `Disassembly` works correctly for 'compressed' (RV32C isa variants) instruction sets.
  - RISC-V stack unwind improvements (stack frames and registers work, variables do not resolve correctly.)
- Fixed a possible endless recursion in the J-Link code, when no chip is connected. (#1123)
- Fixed an issue with ARMv7-a/v8-a where some register values might be corrupted. (#1131)
- Fixed an issue where `probe-rs-cli`'s debug console didn't detect if the core is halted (#1131)
- Fix GDB interface to require a Mutex to enable multi-threaded usage (#1144)
- Debug: RISC-V improvements (#1147).
  - Fix: Variable values now resolve correctly. This fix also fixes variables when using the rustc flag `-Cforce-frame-pointers=off` on ARM.
  - Fix: Allow unwinding past frames with no debug information (See Issue [#896](https://github.com/probe-rs/probe-rs/issues/896))
  - Fix: Using `restart` request from VSCode now works for both states of `halt_after_rest`.
  - Partial Fix: Set breakpoints and step on RISC-V. Breakpoints work but stepping only works for some breakpoints. This will be addressed in a future PR.
- Fix nrf9160 target file so it can erase UICR section (#1151)
- Fix connect under reset for CMSIS-DAP probes(#1159)
- Fix double default algorithms for the stm32f7x line with 1MB flash (#1171)
- Fixed detecting CMSIS-DAP probes that only say "CMSIS-DAP" in interface strings, not the product string (#1142/#1135/#995)

## [0.12.0]

- Added support for `chip-erase` flag under the `probe-rs-cli download` command. (#898)
- Added support for `disable-progressbars` flag under the `probe-rs-cli download` command. (#898)
- Fixed bug in `FlashLoader` not emitting `ProgressEvent::FinishedErasing` when using `do_chip_erase`. (#898)

### Added

- Added initial multicore support. (#565)
- probe-rs-cli-util: added common option structures and logic pertaining to probes and target attachment from cargo-flash. (#723)
- probe-rs-cli-util: escape hatch via `--` for extra cargo options not declared by `common_options::CargoOptions`.
- Added SWDv2 multidrop support for multi-DP chips. (#720)
- Added The possibility to use `--connect-under-reset` for the `probe-rs-cli info` command. (#775)
- Added support for flashing `bin` format binaries with the `probe-rs-cli download` command. (#774)
- Improved number parsing on all the `probe-rs-cli` commands. They now all accept normal (`01234`), hex (`0x1234`), octal (`0o1234`) and binary (`0b1`) formats. (#774)
- Added progress bars to the probe-rs-cli download command. (#776)
- Improve reliability of communication with the RISC-V debug module by recovering from busy errors in batch operations. (#802)
- Added optional ability to load fixed address flashing algorithms (non PIC). (#822)
- Added target definition validation to make handling inside probe-rs easier by making some basic assumptions about the validity of the used `ChipFamily` without always checking again. (#848)
- Added support for the built in JTAG on the ESP32C3 and other ESP32 devices (#863).
- Added name field to memory regions. (#864)
- debugger: Show progress notification while device is being flashed. (#871, #884)
- Add optional ability to load fixed address flashing algorithms (non PIC). (#822)
- Added `probe-rs-cli run` command, to flash and run a binary showing RTT output.
- Added a new USB VID for ST-Link V3 without Mass Storage. (#1070)

### Removed

- probe-rs-cli-util: unused module `argument_handling`. (#760)

### Changed

- Enabled the generation of global timestamps and exception traces for ARM targets on `Session::setup_swv`.
- Changed to `hidraw` for HID access on Linux. This should allow access to HID-based probes without udev rules (#737).
- Support batching of FTDI commands and use it for RISC-V (#717)
- Include the chip string for `NoRamDefined` in its error message
- Improved handling of errors in CMSIS-DAP commands (#745).
- Implemented RTT (String, BinaryLE, and Defmt) in `probe-rs-debugger` (#688).
- `probe-rs-debugger` will use the VSCode Client `launch.json` configuration to set RUST_LOG levels and send output to the VSCode Debug Console (#688).
- Bumped dependencies `bitvec 0.19.4`to `bitvec 0.22`, `nom 6.0.0` to `nom 7.0.0-alpha1`. (#756)
- `DebugProbeError::CommandNotSupportedByProbe` now holds a name string of the unsupported command.
- Target YAMLs: Renamed `core.type` values from `M0, M4, etc` to `armv6m`, `armv7m`, `armv8m`.
- Breaking API: Modify `probe-rs-rtt` interfaces to use `probe_rs::Core` rather than `Arc<Mutex<probe_rs::Session>>`.
- An opaque object is returned to represent a compiled artifact. This allows extra information to be provided
  in future without a breaking change (#795).
- Information on whether a rebuild was necessary is included in the artefact (nothing changed if
  `fresh == true`) (#795).
- `Debug` was reimplemented on `Session` (#795).
- Target YAMLs: Changed `flash_algorithms` from a map to an array. (#813)
- Reject ambiguous chip selection.
- Prefer using `read` over `read_8` for better performance and compatibility. (#829)
- Increased default RTT Timeout (retry waiting for RTT Control Block initialization) to 1000ms in `probe-rs-debugger`. (#847)
- Improved when RTT is initialized/retried, and removed `rtt_timeout` from recognized options of `probe-rs-debugger`. (#850)
- Refactor `probe-rs-debugger` code as per `launch` vs. `attach` changes documented in [VS Code extension PR # 12](https://github.com/probe-rs/vscode/pull/12) (#854)
- Breaking change: `probe-rs-debugger` and the associated [VSCode extension PR #21](https://github.com/probe-rs/vscode/pull/21) now uses camelCase for all `launch.json` properties (#885)
- Publicly export `core::RegisterFile` type.
- The trait surface for DAP/AP/DP access was cleaned up and more clarity around the access level of the API was added by properly putting `Raw` or not in the name.
- `target-gen` now deduplicates flash algorithms when generating target files. (#1010)

### Fixed

- Detect proper USB HID interface to use for CMSIS-DAP v1 probes. Without this, CMSIS-DAP probes with multiple HID interfaces, e.g. MCUlink, were not working properly on MacOS (#722).
- When reading from a HID device, check number of bytes returned to detect USB HID timeouts.
- Fix connecting to EDBG and similar probes on MacOS (#681, #721)
- Fixed incorrect flash range in `fe310` causing flashing to fail (#732).
- Multiple default algorithims would silently select the first, now errors intead (#744).
- Fixed STM32WL targets getting a HardFault when flashing binaries larger than 64K (#762).
- Use a more reliable JTAG IR length detection when there's only a single target in the chain. Fixes an issue with the esp32c3. (#796, #823).
- Replaced `unreachable!` induced panic with logic to fix `probe-rs-debugger` failures. (#847)
- Fixed logic errors and timing of RTT initialization in `probe-rs-debugger`. (#847)
- Debugger: Do not crash the CLI when pressing enter without a command. (#875)
- Fixed panic in CLI debugger when using a command without arguments. (#873)
- Debugger: Reduce panics caused by `unwrap()` usage. (#886)
- probe-rs: When unwinding, detect if the program counter does not change anymore and stop. (#893)

### Target Support

- Added LPC5516 targets. (#853)
- Added LPC552x and LPC55S2x targets. (#742)
- Added SAM3U targets. (#833)
- Added RP2040 target (Raspberry Pi Pico). (#720)
- Added STM32WL55JCIx target. (#835)
- Add esp32.yaml with esp32c3 variant. (#846)
- Added STM32U5 series target.
- Added all RAM regions to most STM32H7 parts. (#864)

## [0.11.0]

### Added

- Support for the `HNONSEC` bit in memory access. This now allows secure access on chips which support TrustZone (#465).
- Support for RISC-V chips which use the System Bus Access method for memory access when debugging (#527).
- Support for double buffering in the flash loader, which increased flashing speed (#107).
- Determine location of debug components by parsing ROM table (#431).
- Support for "flashing" data to RAM in the flash loader (#480).
- Added FTDI C232HM-DDHSL-0 to comaptible USB list for FTDI backend (#485).
- Added `--list-probes` and `-n`option to built-in GDB server binary (#486).
- Added RISC-V support to GDB server (#493).
- Added `Session::target()` to access the target of a session (#497).
- Support for target description in the GDB server (#498).
- Support for register write commands in the GDB server (#510).
- Added `get_target_voltage()` function to `DebugProbe`, which can be used to read the target voltage if the probe supports it (#533).
- Added `do_chip_erase` flag to `DownloadOptions`, to allow using chip erase when flashing (#537).
- RISC-V: Support for memory access using system bus (#527).
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
- Added flash algorithm for GD32VF1 family (#830).

### Changed

- Renamed `MemoryRegion::Flash` to `MemoryRegion::Nvm` (#482).
- Renamed `FlashInfo` to `NvmInfo`
- Renamed `FlashRegion` to `NvmRegion` and its `flash_info()` method to `nvm_info()`
- Renamed `FlashError::NoSuitableFlash` to `FlashError::NoSuitableNvm`
- The `into_arm_interface` and `into_RISC-V_interface` functions are replaced by the `try_into_arm_interface` and
  `try_into_RISC-V_interface` functions, which return the `Probe` struct in the case of an error. This improves the
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
- RISC-V: Use abstract commands for CSR access for improved speed (#487).
- The `download_file` and `download_file_with_options` functions now accept `AsRef<Path>` instead of `&Path`to be more convenient to use (#545, #579).
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
- RISC-V: Fixed scanning for harts (#610).
- RISC-V: Fixed abstract command handling (#611).
- Fixed a bus congestion issue where the chip is polled too often, leading to problems while flashing (#613).
- The breakpoint address is now verified to ensure a breakpoint at the given address is actually possible (#626).
- RISC-V: Use correct address for access to `abstractauto`register (#511).
- The `--chip` argument now works without specifying the `--elf` argument (fix #517).
- Fixed: Invalid "Unable to set hardware breakpoint", by removing breakpoint caching, instead querying core directly (#632)
- Fix crash on unknown AP class. (#662).
- Fix too many chip erases in chips with multiple NvmRegions. (#670).
- Added missing `skip_erase` setter function introduced in #677 (#679).
- Fixed incorrect array size calculation (#683)
- STLink: Removed unnecessary SELECT bank switching (#692)
- STLink: chunk writes in `write_8` to avoid hitting limit (#697)
- Partial fix for a bug where `probe-rs-debugger` does not set breakpoints when the target is in _sleep_ mode (#703)

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
- Added support for implicit ebreak in RISC-V chips (#423, #430).

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
- Support for RISC-V debugging using a Jlink debug probe.
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

[unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.14.2...master
[v0.14.2]: https://github.com/probe-rs/probe-rs/compare/v0.14.1...v0.14.2
[v0.14.1]: https://github.com/probe-rs/probe-rs/compare/v0.14.0...v0.14.1
[v0.14.0]: https://github.com/probe-rs/probe-rs/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/probe-rs/probe-rs/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/probe-rs/probe-rs/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/probe-rs/probe-rs/compare/v0.10.1...v0.11.0
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
