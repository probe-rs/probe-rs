# `probe-run`

> Runs embedded programs just like native ones

`probe-run` is a custom Cargo runner that transparently runs Rust firmware on an embedded device.

`probe-run` is powered by [`probe-rs`] and thus supports all the devices and probes supported by
`probe-rs`.

[`probe-rs`]: https://probe.rs/

## Features

* Acts as a Cargo runner, integrating into `cargo run`.
* Displays program output streamed from the device via RTT.
* Exits the firmware and prints a stack backtrace on hardware breakpoints (e.g. `bkpt`), Rust panics and unrecoverable hardware exceptions (e.g. `HardFault`).

## Installation

To install `probe-run`, use `cargo install probe-run`.

On Linux, you might have to install `libudev` and `libusb` from your package
manager before installing `probe-run`.

``` console
# ubuntu
$ sudo apt install -y libusb-1.0-0-dev libudev-dev

# fedora
$ sudo dnf install -y libusbx-devel systemd-devel
```

### Using the latest `probe-rs`

`probe-run` inherits device support from the `probe-rs` library.
If your target chip does **not** appear in the output of `probe-run --list-chips` it could be that:
1. The latest *git* version `probe-rs` supports your chip but the latest version on crates.io does not.
You could try building `probe-run` against the latest `probe-rs` version; see instructions below.
2. `probe-rs` does not yet support the device.
You'll need to request support in [the `probe-rs` issue tracker](https://github.com/probe-rs/probe-rs/issues).

To build `probe-run` against the latest git version `probe-rs` and install it follow these steps:

``` console
$ # clone the latest version of probe-run; line below uses version v0.3.3
$ git clone --depth 1 --branch v0.3.3 https://github.com/knurling-rs/probe-run

$ cd probe-run

$ # modify Cargo.toml to use the git version of probe-rs
$ # append these lines to Cargo.toml; command below is UNIX-y
$ cat >> Cargo.toml << STOP
[patch.crates-io]
probe-rs = { git = "https://github.com/probe-rs/probe-rs" }
STOP

$ # install this patched version
$ cargo install --path .
```

Note that you may need to modify `probe-rs-rtt` and/or `probe-run` itself to get `probe-run` to compile.
As we only support the crates.io version of `probe-rs` in the unmodified `Cargo.toml` we cannot provide further assistance in that case.

## Setup

**NOTE** for *new* Cargo projects we recommend starting from [`app-template`](https://github.com/knurling-rs/app-template)

### 1. Set the Cargo runner

The recommend way to use `probe-run` is to set as the Cargo runner of your application.

Add these two lines to your Cargo configuration file (`.cargo/config.toml`) and set the particular `--chip` value for your target. In this case it is `nRF52840_xxAA` for the [nRF52840]:

``` toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-run --chip nRF52840_xxAA"
#                          ^^^^^^^^^^^^^
```

To list all supported chips run `probe-run --list-chips`.

#### **1.1 Env variable**

To support multiple devices, or permit overriding default behavior, you may prefer to:
1. set the `${PROBE_RUN_CHIP}` environment variable, and
2. set `runner` (or `CARGO_TARGET_${TARGET_ARCH}_RUNNER`) to `probe-run`:

``` toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-run"
```

#### **1.2 Multiple probes**

If you have several probes connected, you can specify which one to use by adding the `--probe` option to the `runner` or setting the `${PROBE_RUN_PROBE}` environment variable with a value containing either `${VID}:${PID}` or `${VID}:${PID}:${SERIAL}`:

```console
// --probe
$ probe-run --probe '0483:3748' --chip ${PROBE_RUN_CHIP}

// PROBE_RUN_PROBE
$ PROBE_RUN_PROBE='1366:0101:123456' cargo run
```

To list all connected probes, run `probe-run --list-probes`.

[nRF52840]: https://www.nordicsemi.com/Products/Low-power-short-range-wireless/nRF52840

### 2. Enable debug info

Next check that debug info is enabled for all profiles.
If you are using the `cortex-m-quickstart` template then this is already the case.
If not check or add these lines to `Cargo.toml`.

``` toml
[dependencies]
...
panic-probe = { version = "0.2", features = ["print-rtt"] }

# Cargo.toml
[profile.dev]
debug = 1 # default is `true`; not needed if not already overridden

[profile.release]
debug = 1 # default is `false`; using `true` is also OK as symbols reside on the host, not the target
```

### 3. Look out for old dependencies

The `cortex-m` dependency must be version 0.6.3 or newer.
Older versions are not supported.
Check your `Cargo.lock` for old versions.
Run `cargo update` to update the `cortex-m` dependency if an older one appears in `Cargo.lock`.

### 4. Run

You are all set.
You can now run your firmware using `cargo run`.
For example,

``` rust
use cortex_m::asm;
use cortex_m_rt::entry;
use panic_probe as _;
use rtt_target::rprintln;

#[entry]
fn main() -> ! {
    rtt_init_print!(); // You may prefer to initialize another way
    rprintln!("Hello, world!");
    loop { asm::bkpt() }
}
```

would output

``` console
$ cargo run --bin hello
    Finished dev [unoptimized + debuginfo] target(s) in 0.07s
     Running `probe-run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/debug/hello`
  (HOST) INFO  flashing program (30.22 KiB)
  (HOST) INFO  success!
────────────────────────────────────────────────────────────────────────────────
INFO:hello -- Hello, world!
────────────────────────────────────────────────────────────────────────────────
  (HOST) INFO  exiting because the device halted.
To see the backtrace at the exit point repeat this run with
`probe-run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/debug/hello --force-backtrace`
```

## Stack backtraces

When the device raises a hard fault exception, indicating e.g. a panic or a stack overflow, `probe-run` will print a backtrace and exit with a non-zero exit code.

This backtrace follows the format of the `std` backtraces you get from `std::panic!` but includes
`<exception entry>` lines to indicate where an exception/interrupt occurred.

``` rust
#![no_main]
#![no_std]

use cortex_m::asm;
#[entry]
fn main() -> ! {
    // trigger a hard fault exception with the UDF instruction.
    asm::udf()
}
```

``` console
    Finished dev [optimized + debuginfo] target(s) in 0.04s
     Running `probe-run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/debug/hard-fault`
  (HOST) INFO  flashing program (30.08 KiB)
  (HOST) INFO  success!
────────────────────────────────────────────────────────────────────────────────
stack backtrace:
   0: HardFaultTrampoline
      <exception entry>
   1: __udf
   2: cortex_m::asm::udf
        at /<...>/cortex-m-0.6.4/src/asm.rs:104
   3: panic::__cortex_m_rt_main
        at src/bin/hard-fault.rs:12
   4: main
        at src/bin/hard-fault.rs:8
   5: ResetTrampoline
        at /<...>3/cortex-m-rt-0.6.13/src/lib.rs:547
   6: Reset
        at /<...>/cortex-m-rt-0.6.13/src/lib.rs:550
```

If we look at the return code emitted by this `cargo run`, we'll see that it is non-0:

```console
$ echo $?
134
```

⚠️ **NOTE** when you run your application with `probe-run`, the `HardFault` handler (default or user-defined) will *NOT* be executed.

### Backtrace options
#### --backtrace

The `--backtrace` flag is optional and can get passed the following values:

* `--backtrace=always`   - forced backtrace (if you'd like to see a backtrace at the end of successful program run)
* `--backtrace=never`    - suppresed backtrace
* `--backtrace=auto`     - default, shows a backtrace if the program panics or the stack overflows

Run it like this (example for a forced backtrace):

``` console
$ cargo run --bin hello --backtrace=always
```

#### --backtrace-limit

The `--backtrace-limit` flag is optional and defaults to 50. It is possible to set any number.

`--backtrace-limit=0` is accepted and means "no limit".

To show a shortened backtrace showing 5 frames, run:

``` console
$ cargo run --bin panic --backtrace-limit=5
```

Note: if `--backtrace=never` is set, setting `--backtrace-limit` has no effect.

## Troubleshooting

### "Error: no probe was found."

First, check your hardware:

- make sure that your development board has an on-board *hardware* debugger.
If it doesn't, you'll need a separate hardware debugger that works with the JTAG or SWD interface.
Some boards have a USB micro-B or Type-C connector but only come with *bootloader* firmware that lets you load new program over USB Mass Storage, instead of having a dedicated on-board JTAG or SWD to USB chip;
`probe-run` cannot be used with these boards.
- make sure that it is connected to the right port on your development board
- make sure that you are using a **data** cable– some cables are built for charging only! When in doubt, try using a different cable.
- make sure you have the right drivers for the debugger installed (st-link or j-link)

If this doesn't resolve the issue, try the following:

#### [Linux only] udev rules haven't been set

Check if your device shows up in `lsusb`:

```console
$ lsusb
Bus 001 Device 008: ID 1366:1015 SEGGER J-Link
```

If your device shows up like in the example, skip to the next troubleshooting section

**If it doesn't show up**, you need to give your system permission to access the device as a non-root user so that `probe-run` can find your device.

In order to grant these permissions, you'll need to add a new set of udev rules.

To learn how to do this for the nRF52840 Development Kit, check out the [installation instructions](https://embedded-trainings.ferrous-systems.com/installation.html?highlight=udev#linux-only-usb) in our embedded training materials.

afterwards, your device should show up in `probe-run --list-probes` similar to this:

```console
$ probe-run --list-probes
The following devices were found:
[0]: J-Link (J-Link) (VID: 1366, PID: 1015, Serial: <redacted>, JLink)
```

#### No external or on-board debugger present

To use `probe-run` you need a "probe" (also known as "debugger") that sits between your PC and the microcontroller.

Most development boards, especially the bigger ones, have a probe "on-board": If the product description of your board mentions something like a J-Link or ST-Link on-board debugger you're good to go. With these boards, all you need to do is connect your PC to the dev board using a USB cable you are all set to use `probe-run`!

If this is *not* the case for your board, check in the datasheet if it exposes exposes SWD or JTAG pins.
If they are exposed, you can connect a "stand alone" probe device to the microcontroller and then connect the probe to your PC via USB. Some examples of stand alone probes are: the ST-Link and the J-Link.

Note that this may involve some soldering if your board does not come with a pre-attached header to plug your debugger into.

### Error: RTT up channel 0 not found

This may instead present as `Error: RTT control block not found in target memory.`

Your code, or a library you're using (e.g. RTIC) might be putting your CPU to
sleep when idle. You can verify that this is the problem by busy looping instead
of sleeping. When using RTIC, this can be achieved by adding an idle handler to
your app:

```rust
#[idle]
fn idle(_ctx: idle::Context) -> ! {
     loop {}
}
```

Assuming you'd like to still sleep in order to save power, you need to configure
your microcontroller so that RTT can still be handled even when the CPU is
sleeping. How to do this varies between microcontrollers.

On an STM32G0 running RTIC it can be done by amending your init function to set
the `dmaen` bit on `RCC.ahbenr`. e.g.:

```rust
#[init]
fn init(ctx: init::Context) -> init::LateResources {
     ctx.device.RCC.ahbenr.write(|w| w.dmaen().set_bit());
     ...
}
```

### defmt version mismatch

#### end-user
Follow the instructions in the error message to resolve the mismatch.

#### developer
If you are hacking around with `probe-run`, you can disable the version check by setting the `PROBE_RUN_IGNORE_VERSION` environment variable to `true` or `1` at runtime.


## Developer Information

### running your locally modified `probe-run`

For easier copy-paste-ability, here's an example how to try out your local `probe_run` modifications.

```console
$ cd probe-run/
$ PROBE_RUN_IGNORE_VERSION=1 cargo run -- --chip nRF52840_xxAA --backtrace-limit=10 hello
  ˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆ                                   ˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆˆ ˆˆˆˆˆ
  environment variables                                        extra flags             binary to be
  (optional)                                                   (optional)              flashed & run
```

### running snapshot tests

To check whether your change has altered probe-run in unexpected ways, please run the snapshot tests in `tests` before opening a PR if at all possible.

You can do so by connecting a nrf52840 Development Kit and running

```console
$ cargo test -- --ignored
```

## Support Us

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

[Knurling]: https://knurling.ferrous-systems.com
[Ferrous Systems]: https://ferrous-systems.com/
[GitHub Sponsors]: https://github.com/sponsors/knurling-rs
