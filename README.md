# probe-rs
[![crates.io](https://meritbadge.herokuapp.com/probe-rs)](https://crates.io/crates/probe-rs) [![documentation](https://docs.rs/probe-rs/badge.svg)](https://docs.rs/probe-rs) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/!vhKMWjizPZBgKeknOo:matrix.org)

A debugging toolset and library for debugging microchips on a separate host.

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

The `cargo-flash` utility can be used as a cargo subcommand to download a compiled Rust program onto a target device. See https://github.com/probe-rs/cargo-flash for more information.


### CLI

To demonstrate the functionality a small cli was written.
Fire it up with

```
cargo run -p probe-rs-cli -- help
```

The help dialog should then tell you how to use the CLI.

# Usage Examples
## Halting the attached chip

```rust
use probe_rs::Probe;

// Get a list of all available debug probes.
let probes = Probe::list_all();

// Use the first probe found.
let probe = probes[0].open()?;

// Attach to a chip.
let session = probe.attach("nrf52")?;

// Select a core.
let core = session.attach_to_core(0)?;

// Halt the attached core.
core.halt()?;
```

## Reading from RAM

```rust
use probe_rs::Core;

let core = Core::auto_attach("nrf52")?;

// Read a block of 50 32 bit words.
let mut buff = [0u32;50];
core.read_32(0x2000_0000, &mut buff)?;

// Read a single 32 bit word.
let word = core.read_word_32(0x2000_0000)?;

// Writing is just as simple.
let buff = [0u32;50];
core.write_32(0x2000_0000, &buff)?;

// of course we can also write 8bit words.
let buff = [0u8;50];
core.write_8(0x2000_0000, &buff)?;
```

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