# `probe-run`

> Runs embedded programs just like native ones

`probe-run` is a Cargo runner that runs embedded programs on your Cortex-M
microcontroller.

## Features

* Acts as a Cargo runner, integrating into `cargo run`.
* Displays program output streamed from the device via RTT.
* Exits the firmware and prints a stack backtrace on breakpoints.

## Installation

To install `probe-run`, use `cargo install probe-run`.

On Linux, you might have to install `libudev` and `libusb` from your package
manager before installing `probe-run`.

To use it in your project, create or modify an existing `.cargo/config` like
follows:

```toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-run --chip nRF52840_xxAA" # <- add this
```

Replace `nRF52840_xxAA` with your target chip. To see a list of supported chips,
see the output of `probe-run --list-chips`.

To print to the host console, the firmware needs to use an RTT implementation
like [`rtt-target`]. To trigger a backtrace and exit the application, a `bkpt`
instruction needs to be executed (eg. via `cortex_m::asm::bkpt`).

[`rtt-target`]: https://crates.io/crates/rtt-target

## Support

`probe-run` is part of the [Knurling] project, [Ferrous Systems]' effort at
improving tooling used to develop for embedded systems.

If you think that our work is useful, consider sponsoring it via [GitHub
Sponsors].

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)

- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
licensed as above, without any additional terms or conditions.

[Knurling]: https://github.com/knurling-rs/meta
[Ferrous Systems]: https://ferrous-systems.com/
[GitHub Sponsors]: https://github.com/sponsors/knurling-rs
