# probe-rs
[![crates.io](https://meritbadge.herokuapp.com/probe-rs)](https://crates.io/crates/probe-rs) [![documentation](https://docs.rs/probe-rs/badge.svg)](https://docs.rs/probe-rs) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

A debugging toolset and library for debugging ARM cores on a separate host.

## Motivation

The goal of this library is to provide a toolset to interact with a variety of embedded MCUs and debug probes.
For starters, ARM cores will be supported through use of the CoreSight protocol.
If there is high demand and more contributors, it is intended to add support for other architectures.

Similar projects like OpenOCD, PyOCD, Segger Toolset, ST Tooling, etc. exist.
They all implement the GDB protocol and their own protocol on top of it to enable GDB to communicate with the debug probe.
This is not standardized and also little bit unstable sometimes. For every tool the commands are different and so on.

This project gets rid of the GDB layer and provides a direct interface to the debug probe,
which then enables other software, for example [VisualStudio](https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website) to use it's debug functionality.

What's more is that we can use CoreSight to its full extent. We can trace and modify memory as well as registers in real time.

**The end goal is a complete library toolset to enable other tools to use the functionality of CoreSight.**

## Functionality

- The lib can connect to a DAPLink or an STLink and read and write memory.
- It can read ROM tables and thus extract CoreSight component information.
- It can download ELF binaries using standard ARM flash blobs.
- Basic debugging (attach, reset, halt, step, show stacktrace, add breakpoint, halt on breakpoint) works.

Focus of the development is having a full implementation (CoreSight, Flashing, Debugging) working for the DAPLink and go from there.

### Downloading a file

For downloading an ELF binary (standard cargo build output), you should use the `cargo-flash` utility.
You can install it via `cargo install cargo-flash` and then use it via `cargo flash` or just use your own built `cargo-flash`. `cargo flash` is just an alias for `cargo-flash`.

#### Basic

Use `cargo flash` to build your binary and download the ELF binary to your target.

The following example shows the necessary steps using 
a [BBC micro:bit](https://microbit.org/) as the target board, and an example from 
the [microbit](https://github.com/therealprof/microbit) crate.

First, ensure that you have a local copy of the crate:

```console
$ git clone https://github.com/therealprof/microbit.git && cd microbit/
Cloning into 'microbit'...
remote: Enumerating objects: 31, done.
remote: Counting objects: 100% (31/31), done.
remote: Compressing objects: 100% (21/21), done.
remote: Total 423 (delta 15), reused 15 (delta 10), pack-reused 392
Receiving objects: 100% (423/423), 94.83 KiB | 57.00 KiB/s, done.
Resolving deltas: 100% (282/282), done.
```

In the `microbit` folder, you can now run
`cargo flash` to compile and flash it in a single step:

```console
$ RUST_LOG=info cargo flash --release --example  gpio_hal_ledbutton  --chip  nRF51822_xxAA
    Finished release [optimized + debuginfo] target(s) in 0.06s
    Flashing ./target/thumbv6m-none-eabi/release/examples/gpio_hal_ledbutton
 INFO  probe_rs::probe::daplink > Attaching to target system (clock = 1000000)
 INFO  probe_rs::probe::daplink > Debug Port Version:  DPv1
 INFO  probe_rs::probe::daplink > Debug Port Designer: ARM Ltd
 INFO  probe_rs::probe::daplink > Succesfully attached to system and entered debug mode
RUN INIT
 INFO  probe_rs::flash::flasher > Erasing sector at address 0x00000000.
 INFO  probe_rs::flash::flasher > Done erasing sector. Result is 0. This took 113.665878ms
RUN INIT
 INFO  probe_rs::flash::flasher > Flashing one page of size: 1024
 INFO  probe_rs::flash::flasher > Flashing took: 723.845166ms
    Finished in 6.507s
```

Now a single LED should blink on your micro:bit.


#### Full set of commands

You can pass some arguments you could pass to `cargo build` to configure your build target. Use `cargo flash --help` for a list of available commands.

#### Autodetection

If you do not pass a target via `cargo flash --target <target-triplet>` probe-rs will try to identify your target. If this doesn't succeed, please specify your target manually.

### CLI

To demonstrate the functionality a small cli was written.
Fire it up with

```
cargo run -p probe-rs-cli -- help
```

The help dialog should then tell you how to use the CLI.

## Roadmap

- [ ] v0.3.0
  - [x] Automatic CMSIS-Pack parsing and loading for flash algorithms.
- [ ] ...
  - [ ] Basic debugging for Cortex m0, m3, m4.
    - [x] Stepping
    - [x] Halting
    - [X] Breaking
    - [x] Running
  - [ ] Basic VSCode plugin
    - [x] Stepping
    - [x] Halting
    - [ ] Breaking
    - [x] Running
    - [ ] Variable inspection
      - [x] Pointers/References
      - [ ] Structs
      - [ ] Enums
      - [ ] Basic types
    - [x] Stack Trace display
    - [ ] Stackframe display
  - [ ] Basic CLI
    - [x] Stepping
    - [x] Halting
    - [X] Breaking
    - [x] Running
  - [ ] Semihosting.
  - [ ] Tracing.
  - [ ] SVD file support.

## FAQ

### I need help!

Don't hesitate to [file an issue](https://github.com/probe-rs/probe-rs/issues/new), ask questions on [matrix](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org?via=matrix.org&via=spodeli.org), or contact [@Yatekii](https://github.com/Yatekii) by e-mail.

### How can I help?

Please have a look at the issues or open one if you feel that something is needed.

Any contibutions are very welcome!

Also have a look at [CONTRIBUTING.md](https://github.com/Yatekii/probe-rs/blob/master/CONTRIBUTING.md).

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT) at your option.

### Acknowledgements

This crate contains code (the flash algorithms) that's highly based on the code of the [pyOCD](https://github.com/mbedmicro/pyOCD) project.
Some of this code might reside in the `ocd::probe::flash` module and is subject to the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0) terms.

### Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
