# `probe-run`

> Runs embedded programs just like native ones

`probe-run` is a custom Cargo runner that transparently runs Rust firmware on a
remote device.

`probe-run` is powered by [`probe-rs`] and thus supports as many devices and probes as
`probe-rs` does.

[`probe-rs`]: https://probe.rs/

## Features

* Acts as a Cargo runner, integrating into `cargo run`.
* Displays program output streamed from the device via RTT.
* Exits the firmware and prints a stack backtrace on breakpoints.

## Installation

To install `probe-run`, use `cargo install probe-run`.

On Linux, you might have to install `libudev` and `libusb` from your package
manager before installing `probe-run`.

## Setup

1. Set the Cargo runner

The recommend way to use `probe-run` is to set as the Cargo runner of your application.
Add this line to your Cargo configuration (`.cargo/config`) file:


``` toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-run --chip $CHIP"
```

Instead of `$CHIP` you'll need to write the name of your microcontroller.
For example, one would use `nRF52840_xxAA` for the nRF52840 microcontroller.
To list all supported chips run `probe-run --list-chips`.

2. Enable debug info

Next check that debug info is enabled for all profiles.
If you are using the `cortex-m-quickstart` template then this is already the case.
If not check or add these lines to `Cargo.toml`.

``` toml
# Cargo.toml
[profile.dev]
debug = 1 # default is `true`; not needed if not already overridden

[profile.release]
debug = 1 # default is `false`; using `true` is also OK
```

3. Look out for old dependencies

The `cortex-m` dependency must be version 0.6.3 or newer.
Older versions are not supported.
Check your `Cargo.lock` for old versions.
Run `cargo update` to update the `cortex-m` dependency if an older one appears in `Cargo.lock`.

4. Run

You are all set.
You can now run your firmware using `cargo run`.
For example,

``` rust
use cortex_m::asm;
use cortex_m_rt::entry;
use rtt_target::rprintln;

#[entry]
fn main() -> ! {
    // omitted: rtt initialization
    rprintln!("Hello, world!");
    loop { asm::bkpt() }
}
```

``` console
$ cargo run --bin hello
Running `probe-run target/thumbv7em-none-eabi/debug/hello`
flashing program ..
DONE
resetting device
Hello, world!
stack backtrace:
0: 0x0000031e - __bkpt
1: 0x000001d2 - hello::__cortex_m_rt_main
2: 0x00000108 - main
3: 0x000002fa - Reset
```

## Stack backtraces

When the firmware reaches a BKPT instruction the device halts. The `probe-run` tool treats this
halted state as the "end" of the application and exits with exit-code = 0. Before exiting,
`probe-run` prints the stack backtrace of the halted program.

This backtrace follows the format of the `std` backtraces you get from `std::panic!` but includes
`<exception entry>` lines to indicate where an exception/interrupt occurred.

``` rust
use cortex_m::asm;
use rtt_target::rprintln;
#[entry]
fn main() -> ! {
    // omitted: rtt initialization
    rprintln!("main");
    SCB::set_pendsv();
    rprintln!("after PendSV");
    loop { asm::bkpt() }
}
#[exception]
fn PendSV() {
    rprintln!("PendSV");
    asm::bkpt()
}
```

``` console
$ cargo run --bin exception --release
main
PendSV
stack backtrace:
0: 0x00000902 - __bkpt
<exception entry>
1: 0x000004de - nrf52::__cortex_m_rt_main
2: 0x00000408 - main
3: 0x000005ee - Reset
```


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
