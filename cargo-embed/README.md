# cargo-embed

[![crates.io](https://meritbadge.herokuapp.com/cargo-embed)](https://crates.io/crates/cargo-embed) [![documentation](https://docs.rs/cargo-embed/badge.svg)](https://docs.rs/cargo-embed) [![Actions Status](https://github.com/probe-rs/cargo-embed/workflows/CI/badge.svg)](https://github.com/probe-rs/cargo-embed/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

This crate provides a cargo subcommand to work with embedded targets.

It can flash targets, just like cargo-flash but can do much more, such as logging RTT output from the target, opening a GDB server connected to the target, and much more functionality such as ITM to come!

Various chip families including but not limited to nRF5x, STM32 and LPC800 can be worked with using DAPLink, ST-Link or J-Link.
It supports all the targets & probes [probe-rs](https://github.com/probe-rs/probe-rs) supports.

## Installation

You can install this utility with cargo:

```bash
cargo install cargo-embed
```

## Usage

You can use it like any cargo command would be used

```bash
cargo embed <args>
```

which will then build your binary and download the contents onto the connected target.

## Configuration

You can configure `cargo-embed` with a file called `Embed.toml` in your project directory.
Instead of a TOML file, you can also use a YAML or a JSON file. Choose what suits you best!

You can find all available options in the [default.toml](src/config/default.toml). Commented out options are the ones that are `None` by default.