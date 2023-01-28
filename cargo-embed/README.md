# cargo-embed

[![crates.io](https://img.shields.io/crates/v/cargo-embed.svg)](https://crates.io/crates/cargo-embed) [![Actions Status](https://img.shields.io/github/actions/workflow/status/probe-rs/probe-rs/ci.yml?branch=master)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

This crate provides a cargo subcommand to work with embedded targets.

It can flash targets, just like cargo-flash but can do much more, such as logging RTT output from the target, opening a GDB server connected to the target, and much more functionality such as ITM to come!

Various chip families including but not limited to nRF5x, STM32 and LPC800 can be worked with using DAPLink, ST-Link or J-Link.
It supports all the targets & probes [probe-rs](https://github.com/probe-rs/probe-rs) supports.

## Support

If you think `cargo-embed` makes your embedded journey more enjoyable or even earns you money, please consider supporting the project on [Github Sponsors](https://github.com/sponsors/probe-rs/) for better support and more features.

## Installation

You can install this utility with cargo after installing the prerequisites listed below:

```bash
cargo install cargo-embed
```

## Usage

You can use it like any cargo command would be used

```bash
cargo embed <args>
```

This will do following in sequence:

1. build your binary
2. detect a probe
3. (if enabled) upload the contents onto the connected target
4. (if enabled) reset the target
5. (if enabled) start RTT host side
6. (if enabled) start gdb debugging

### RTT UI

If you've enabled rtt (see [Configuration](#configuration)) the RTT UI will appear automatically.

#### Keyboard shortcuts

| Command       | Action                                         |
| ------------- | ---------------------------------------------- |
| `^c`          | Quit                                           |
| `Fn{n}`       | Switch to tab `n`                              |
| Any character | Add the character to the pending input         |
| `Backspace`   | Delete the last character of the pending input |
| `Enter`       | Send the pending input                         |
| `PgUp`        | Scroll up                                      |
| `PgDn`        | Scroll Down                                    |
| `^l`          | Clear current tab                              |

## Configuration

You can configure `cargo-embed` with a file called `Embed.toml` (or `.embed.toml`) in your project directory. That file should be added to your git history.

For local-only configuration overrides, you can create an `Embed.local.toml` (or `.embed.local.toml`) file and add that to your `.gitignore`.

Config file precedence:

1. `Embed.local.*`
2. `.embed.local.*`
3. `Embed.*`
4. `.embed.*`
5. Default configuration

Instead of a TOML file, you can also use a JSON or YAML file. Choose what suits you best!

You can find all available options in the [default.toml](https://github.com/probe-rs/probe-rs/blob/master/cargo-embed/src/config/default.toml). Commented out options are the ones that are `None` by default.

## Building

`cargo-embed` can be built using cargo, after installing the necessary prerequisites. See the list below for your operating
system.

### FTDI Support

FTDI support is optional. You can enable it with the `ftdi` feature. You also need the correct prerequisites from the next section installed.

### Prerequisites

cargo-embed depends on the [libusb](https://libusb.info/) and optionally on [libftdi](https://www.intra2net.com/en/developer/libftdi/) libraries, which need to be installed to build cargo-embed.

#### Linux

On Ubuntu or Debian, the following packages need to be installed:

```
> sudo apt install -y pkg-config libusb-1.0-0-dev libftdi1-dev libudev-dev
```

For Fedora or CentOS

```
> dnf install systemd-devel
```

You may also need to remove old versions of libusb:

```
> sudo apt remove libusb-dev
```

#### Windows

On Windows you can use [vcpkg](https://github.com/microsoft/vcpkg#quick-start-windows) to install the prerequisites:

```
# dynamic linking 64-bit
> vcpkg install libftdi1:x64-windows libusb:x64-windows
> set VCPKGRS_DYNAMIC=1

# static linking 64-bit
> vcpkg install libftdi1:x64-windows-static-md libusb:x64-windows-static-md
```

#### macOS

On macOS, [homebrew](https://brew.sh/) is the suggested method to install libftdi:

```
> brew install libftdi
```

# Sentry logging

We use Sentry to record crash data. This helps us trace crashes better.
No data will ever be transmitted without your consent!
All data is transmitted completely anonymously.
This is an OPT-IN feature. On crash, cargo-embed will ask you whether to transmit the data or not. You can also set whether to do this for all times with an environment variable to omit the message in the future.
If you do not wish to have sentry integrated at all, you can also build cargo-embed without sentry by using `--no-default-features`.
