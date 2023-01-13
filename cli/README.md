# probe-rs-cli

[![crates.io](https://img.shields.io/crates/v/probe-rs-cli.svg)](https://crates.io/crates/probe-rs-cli) [![documentation](https://docs.rs/probe-rs-cli/badge.svg)](https://docs.rs/probe-rs-cli) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

This crate provides a CLI to work with embedded targets.

You can use it as a cargo runner, flash targets, run target diagnostics, has a simple debugger, can log RTT output from the target, opening a GDB server connected to the target, and much more functionality!

Various chip families including but not limited to nRF5x, STM32 and LPC800 can be worked with using DAPLink, ST-Link or J-Link.
It supports all the targets & probes [probe-rs](https://github.com/probe-rs/probe-rs) supports.

## Support

If you think the `probe-rs-cli` makes your embedded journey more enjoyable or even earns you money, please consider supporting the project on [Github Sponsors](https://github.com/sponsors/probe-rs/) for better support and more features.

## Installation

You can install this utility with cargo after installing the prerequisites listed below:

```bash
cargo install probe-rs-cli
```

## Usage

You can discover the available functionality

```bash
probe-rs-cli help
```
