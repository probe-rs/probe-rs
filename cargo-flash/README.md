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

## Add more chip definitions

If you have a chip you want to flash, feel free to contribute to [probe-rs](https://github.com/probe-rs/probe-rs).