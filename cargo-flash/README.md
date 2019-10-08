# cargo-flash

This crate provides an utility to flash ARM chips.

As of writing this, flashing works for the **nRF51** using a **DAPLink** or an **ST-Link** only.

## Installation

You can install this utility with

`cargo install cargo-flash`

## Usage

You can use it like any cargo command would be used

`cargo flash <args>`

which will then build your binary and flash the contents onto the connected nRF51.

### Examples

`cargo flash --release --chip nRF51822 --target thumbv6m-none-eabi --example gpio_hal_blinky`

`cargo flash --release --chip-description-path ../../.config/probe-rs/targets/nRF52840.yaml --target thumbv6m-none-eabi --example gpio_hal_blinky`

### Manually selecting a chip

To manually select a chip, you can use the `--chip <chip name>` argument. The chip name is an identifier such as `nRF51822` or `STM32F042`. Capitalization does not matter; Special characters do matter.

### Specifying the chip via chip configuration file

You can directly set the chip description by using the `--chip-description-path <chip description file path>` argument. You need to pass it the path to a valid yaml chip description.

### Locally installing & overriding chip descripions

You can install valid chip description files locally under `$HOME/.config/probe-rs/targets`. Any chip descriptions for identifiers that match the compiled in identifiers will replace the compiled in descriptions. You can override all the descriptions like this. Invalid files will be ignored gracefully. Use `RUST_LOG=info` to see if there are any errors.

## Add more chip definitions

If you have a chip you want to flash, feel free to contribute to [probe-rs](https://github.com/probe-rs/probe-rs).