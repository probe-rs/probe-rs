# probe-rs

a modern, embedded debugging toolkit,
written in Rust

[![crates.io](https://meritbadge.herokuapp.com/probe-rs)](https://crates.io/crates/probe-rs) [![documentation](https://docs.rs/probe-rs/badge.svg)](https://docs.rs/probe-rs) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/#probe-rs:matrix.org)

The goal of this library is to provide a toolset to interact with a variety of embedded MCUs and debug probes.

Similar projects like OpenOCD, PyOCD, Segger Toolset, ST Tooling, etc. exist.
They all implement the GDB protocol and their own protocol on top of it to enable GDB to communicate with the debug probe.
Only Segger provides a closed source DLL which you can use for talking to the JLink.

This project gets rid of the GDB layer and provides a direct interface to the debug probe,
which then enables other software to use it's debug functionality.

**The end goal of this project is to have a complete library toolset to enable other tools to communicate with embedded targets.**

## Functionality

As of 0.6.0 this lib can

- connect to a DAPLink, STLink or JLink
- talk to ARM and Risc-V cores via SWD or JTAG
- read and write arbitrary memory of the target
- halt, run, step, breakpoint and much more the core
- download ELF, BIN and IHEX binaries using standard CMSIS-Pack flash algorithms to ARM cores
- provide debug information about the target state (stacktrace, stackframe, etc)

To see what new functionality was added have a look at the [CHANGELOG](CHANGELOG.md)

## Downloading a file

The `cargo-flash` utility can be used as a cargo subcommand to download a compiled Rust program onto a target device. It can also be used to download arbitrary ELF files that might come out of a C/C++ compiler. Have a look at [cargo-flash](https://github.com/probe-rs/cargo-flash) for more information.

## GDB

We provide a GDB stub you can use until [Microsoft DAP](https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website) support is fully implemented.
You can find it [here](https://github.com/probe-rs/probe-rs/tree/master/gdb-server) and you can also use it from within `cargo-flash` with the `--gdb` flag.

## VScode

We are implementing [Microsoft DAP](https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website) to provide full probe-rs integration into modern debuggers such as the built in one of VSCode.

## Usage Examples
### Halting the attached chip

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

### Reading from RAM

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

Don't hesitate to [file an issue](https://github.com/probe-rs/probe-rs/issues/new), ask questions on [Matrix](https://matrix.to/#/#probe-rs:matrix.org), or contact [@Yatekii](https://github.com/Yatekii) via e-mail.

### How can I help?

Please have a look at the issues or open one if you feel that something is needed.

Any code contibutions are very welcome!

Also have a look at [CONTRIBUTING.md](CONTRIBUTING.md).

### Our company needs feature X and would pay for it's development

Please reach out to [@Yatekii](https://github.com/Yatekii)

## Sponsors

[![Technokrat](https://technokrat.ch/static/img/svg_banner-light.svg)](https://technokrat.ch)

## Acknowledgements

In early stages of this library, we profited invaluably from the pyOCD code to understand how flashing works. Also it's always a good reference to cross check how ARM specific things work. So, a big thank you to the team behind pyOCD!

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT) at your option.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.