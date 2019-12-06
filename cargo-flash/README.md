# cargo-flash

[![crates.io](https://meritbadge.herokuapp.com/cargo-flash)](https://crates.io/crates/cargo-flash) [![documentation](https://docs.rs/cargo-flash/badge.svg)](https://docs.rs/cargo-flash) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

This crate provides an utility to flash ELF binaries onto ARM chips.

As of writing this, flashing works for the **nRF51822, nRF52832, nRF52840, STMF042, STMF429xI** using a **DAPLink** or an **ST-Link**.

## Installation

You can install this utility with

`cargo install cargo-flash`

## Usage

You can use it like any cargo command would be used

`cargo flash <args>`

which will then build your binary and download the contents onto the connected target.

### Examples

#### Autodetecting the correct chip target and use the default flags

`cargo flash`

#### Specifying manually what options should be used

`cargo flash --release --chip nRF51822 --target thumbv6m-none-eabi --example gpio_hal_blinky`

#### Use a custom chip definition from a non-builtin file

`cargo flash --release --chip-description-path nRF51822.yaml --target thumbv6m-none-eabi --example gpio_hal_blinky`

### Manually selecting a chip

To manually select a chip, you can use the `--chip <chip name>` argument. The chip name is an identifier such as `nRF51822` or `STM32F042`. Capitalization does not matter; Special characters do matter.

### Specifying a chip family description file

You can add a temporary chip family description by using the `--chip-description-path <chip description file path>` or `-c` argument. You need to pass it the path to a valid yaml family description.
All the targets of the family will then be added to the registry temporarily and will override existing variants with the same name.
You can use this feature to tinker with a chip family description until it works and then submit it to upstream for inclusion.

### Extracting a chip family description file from a CMSIS-Pack

You can extract the family description file by running [target-gen](https://github.com/probe-rs/target-gen) on a `.pack` file with `cargo run -- file.pack out_dir`. You can obtain the pack from ARM for example. Their online [registry](https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search) is a good start :)
You can also reference to an already unziped `pack` directory instead of the `file.pack` archive file.

## Add more chip definitions

If you have a chip you want to flash, feel free to contribute to [probe-rs](https://github.com/probe-rs/probe-rs).
