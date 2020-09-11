# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

## [0.9.1]

### Added

- Added a config flag to config what format to use on a channel.

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

[Unreleased]: https://github.com/probe-rs/probe-rs/compare/v0.9.1...master
[0.9.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.9.1..v0.9.0
[0.9.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.9.0..v0.8.0
[0.8.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.8.0..v0.7.0
[0.7.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.7.0..v0.6.1
[0.6.1]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.1..v0.6.0
[0.6.0]: https://github.com/probe-rs/probe-rs/releases/tag/v0.6.0
