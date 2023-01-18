# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.2]

Released 2023-01-18

## [0.14.1]

Released 2023-01-14

## [0.14.0]

Released 2023-01-13

### Added

### Changed

### Fixed

## [0.13.0]

### Changed

- Update to probe-rs 0.13.0.

## [0.12.0]

### Changed

- Update to probe-rs 0.12.0.

## [0.11.0]

### Added

- RTT channels can be configured to use one of three blocking modes
- RTT and GDB server can now run concurrently (#159).

### Changed

- Update to probe-rs 0.11.0.
- Improved handling of config files. Unknown keys in the config file now cause an error, and trying to use unknown profiles as well (#205).

## [0.10.1]

### Changed

- Disable sentry by default as an intermediate measure to fix the subpar user experience due to its introduction.

## [0.10.0]

### Added

- Updated to probe-rs 0.10.0. Please consult its own changelog for new features and fixes.
- Added logging to sentry.io. This is 100% OPT-IN! You will be asked only if an unhandled error or panic occurs, and only if you give consent, data is transmitted. If you do not trust us either way, you can disable the `sentry` feature when you install the crate. The completely anonymous data can be investigated on sentry.io by anyone who likes to see it. Unfortunately sentry.io does not feature public orgs yet, so please reach out to @Yatekii to be added.
  Sentry helps us track down tricky issues that only occur in very specific cases. It is very much appreciated if you log upcoming errors this way (#125)!
- Added printing of the git hash cargo-embed was compiled with and the current package version (#116).

### Changed

- FTDI support is now optional. To enable FTDI support, please use the `ftdi` feature (#131).

## [0.9.1]

### Added

- Added a config flag to config what format to use on a channel.
- Added support for Defmt over RTT

### Changed

### Fixed

- Fixed a bug where all channels except 0 would be interpreted as binary.

## [0.9.0]

### Added

- The config supports a new section called `reset`. It controls whether the target is reset. Default config:

  ```toml
  [default.reset]
  # Whether or not the target should be reset.
  # When flashing is enabled as well, the target will be reset after flashing.
  enabled = true
  # Whether or not the target should be halted after reset.
  halt_afterwards = false
  ```

  This way, you can add a `cargo embed` target that allows resetting and
  optionally halting without flashing. Useful for debugging.

- Improved logging on different levels.
- Added the possibility to save logs (#28).
- Added support for cargo workspaces with the replacement of `cargo-project` with `cargo-metadata`.
- Added a flag to override the selected chip with `--chip`.
- Added a flag to override the selected probe with `--probe`.

### Changed

- The config option `flashing.halt_afterwards` has moved to `reset.halt_afterwards`

### Fixed

- Fixed the enter key for text input in the RTT terminal.
- Fixed loading of local config files.
- Fixed the default.toml.
- Fixed the error message when multiple probes are detected.

## [0.8.0]

### Added

- Add Windows support with the help of crossterm instead of termion.
- Introduced deriveable configs. With deriveable configs it is possible to create multible configs and derive parts of a config from another.
  An example is this config:

      ```toml
      [rtt.rtt]
      enabled = true

      [rtt.gdb]
      enabled = false

      [gdb.rtt]
      enabled = false

      [gdb.gdb]
      enabled = true
      ```

      This creates a config which has three configs:
      - The default one with the prefix "default" as found in [default.toml](src/config/default.toml)
      - A config with the prefix "rtt" which inherits from "default" implicitely (use general.derives = "prefix" to derive from a specific config) which has RTT enabled but GDB disabled.
      - A config with the prefix "gdb" which inherits from "default" implicitely (use general.derives = "prefix" to derive from a specific config) which has GDB enabled but RTT disabled.
      To use a specific config, call `cargo-embed prefix`.
      NOTE: This is a congig breaking change! You must update your `Embed.toml` configs!

### Changed

- The `probe.probe_selector` property is now split into three properties:
  - usb_vid
  - usb_pid
  - serial
- The `RUST_LOG` environment variable can now override the log level set in the config.
- Improved errors by a large margin by properly displaying the stacked errors with the help of anyhow.

### Fixed

- Panics in app that could occur due to a bug will no longer mess up the user's terminal.
- Fixed a bug where the progress bars from the flashing procedure would swallow all log messages.
- Fixed a bug where the RTT UI would panic if no channels were configured.

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

[unreleased]: https://github.com/probe-rs/cargo-embed/compare/v0.14.2...master
[v0.14.2]: https://github.com/probe-rs/cargo-embed/compare/v0.14.1...v0.14.2
[v0.14.1]: https://github.com/probe-rs/cargo-embed/compare/v0.14.0...v0.14.1
[v0.14.0]: https://github.com/probe-rs/cargo-embed/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.12.0..v0.13.0
[0.12.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.11.0..v0.12.0
[0.11.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.10.1..v0.11.0
[0.10.1]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.10.0..v0.10.1
[0.10.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.9.0..v0.10.0
[0.9.1]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.9.0..v0.9.1
[0.9.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.8.0..v0.9.0
[0.8.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.7.0..v0.8.0
[0.7.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.6.1..v0.7.0
[0.6.1]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.6.0..v0.6.1
[0.6.0]: https://github.com/probe-rs/cargo-embed/releases/tag/v0.6.0
