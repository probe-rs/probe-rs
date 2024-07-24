# probe-rs

a modern, embedded debugging toolkit,
written in Rust

[![crates.io](https://img.shields.io/crates/v/probe-rs)](https://crates.io/crates/probe-rs) [![documentation](https://docs.rs/probe-rs/badge.svg)](https://docs.rs/probe-rs) [![Actions Status](https://img.shields.io/github/actions/workflow/status/probe-rs/probe-rs/ci.yml?branch=master)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/#probe-rs:matrix.org)

The goal of this library is to provide a toolset to interact with a variety of embedded MCUs and debug probes.

Similar projects like OpenOCD, PyOCD, Segger Toolset, ST Tooling, etc. exist.
They all implement the GDB protocol and their own protocol on top of it to enable GDB to communicate with the debug probe.
Only Segger provides a closed source DLL which you can use for talking to the JLink.

This project gets rid of the GDB layer and provides a direct interface to the debug probe,
which then enables other software to use its debug functionality.

**The end goal of this project is to have a complete library toolset to enable other tools to communicate with embedded targets.**

## Functionality

As of version 0.10.0 this library can

- connect to a DAPLink, STLink or JLink
- talk to ARM and Risc-V cores via SWD or JTAG
- read and write arbitrary memory of the target
- halt, run, step, breakpoint and much more the core
- download ELF, BIN and IHEX binaries using standard CMSIS-Pack flash algorithms to ARM cores
- provide debug information about the target state (stacktrace, stackframe, etc.)

To see what new functionality was added have a look at the [CHANGELOG](CHANGELOG.md)

## Support

If you think probe-rs makes your embedded journey more enjoyable or even earns you money, please consider supporting the project on [Github Sponsors](https://github.com/sponsors/probe-rs/) for better support and more features.

## Tools

In addition to being a library, probe-rs also includes a suite of tools which can be used for flashing and debugging.

### Installation

The recommended way to install the tools is to download a precompiled version, using one of the methods below.
See <https://probe.rs/docs/getting-started/installation/> for a more detailed guide.

### cargo-flash

The `cargo-flash` utility can be used as a cargo subcommand to download a compiled Rust program onto a target device. It can also be used to download arbitrary ELF files that might come out of a C/C++ compiler. Have a look at [cargo-flash](https://probe.rs/docs/tools/cargo-flash/) for more information.

### cargo-embed

If you are looking for a more extended debugging experience, please have a look at [cargo-embed](https://probe.rs/docs/tools/cargo-embed/) which provides support for GDB, RTT, and config files.

### Editors and IDEs

We have implemented the [Microsoft Debug Adapter Protocol (DAP)](https://microsoft.github.io/debug-adapter-protocol/). This makes embedded debugging via probe-rs available in modern code editors implementing the standard, such as VSCode. The DAP website includes [a list of editors and IDEs which support DAP](https://microsoft.github.io/debug-adapter-protocol/implementors/tools/).

#### VSCode

The probe-rs website includes [VSCode configuration instructions](https://probe.rs/docs/tools/debugger/).

## Usage Examples

### Halting the attached chip

```rust
use probe_rs::probe::{list::Lister, Probe};
use probe_rs::Permissions;

fn main() -> Result<(), probe_rs::Error> {
    // Get a list of all available debug probes.
    let lister = Lister::new();

    let probes = lister.list_all();

    // Use the first probe found.
    let mut probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("nRF52840_xxAA", Permissions::default())?;

    // Select a core.
    let mut core = session.core(0)?;

    // Halt the attached core.
    core.halt(std::time::Duration::from_millis(10))?;

    Ok(())
}
```

### Reading from RAM

```rust
use probe_rs::{MemoryInterface, Permissions, Session};

fn main() -> Result<(), probe_rs::Error> {
    // Attach to a chip.
    let mut session = Session::auto_attach("nRF52840_xxAA", Permissions::default())?;

    // Select a core.
    let mut core = session.core(0)?;

    // Read a block of 50 32 bit words.
    let mut buff = [0u32; 50];
    core.read_32(0x2000_0000, &mut buff)?;

    // Read a single 32 bit word.
    let word = core.read_word_32(0x2000_0000)?;

    // Writing is just as simple.
    let buff = [0u32; 50];
    core.write_32(0x2000_0000, &buff)?;

    // of course we can also write 8bit words.
    let buff = [0u8; 50];
    core.write_8(0x2000_0000, &buff)?;

    Ok(())
}
```

## FAQ

### I need help!

Don't hesitate to [file an issue](https://github.com/probe-rs/probe-rs/issues/new), ask questions on [Matrix](https://matrix.to/#/#probe-rs:matrix.org), or contact [@Yatekii](https://github.com/Yatekii) via e-mail.

There is also a [troubleshooting section](https://probe.rs/docs/knowledge-base/troubleshooting/) on the [project page](https://probe.rs/).

### How can I help?

Please have a look at the issues or open one if you feel that something is needed.

Any contributions are very welcome!

Also have a look at [CONTRIBUTING.md](CONTRIBUTING.md).

### Our company needs feature X and would pay for its development

Please reach out to [@Yatekii](https://github.com/Yatekii)

### Building

Building requires Rust and Cargo which can be installed [using rustup](https://rustup.rs/). On Linux these can be installed with your package manager:

```console
# Ubuntu
> sudo apt install -y libudev-dev

# Fedora
> sudo dnf install -y libudev-devel
```

### Adding Targets

Target files are generated using [target-gen](https://github.com/probe-rs/probe-rs/tree/master/target-gen) from CMSIS packs provided [here](https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search).
Generated files are then placed in `probe-rs/targets` for inclusion in the probe-rs project.

### Writing new flash algorithms

If there is no CMSIS-Pack with a flash algorithm available, it is necessary to write a target definition and a flash algorithm by oneself.
You can use our [template](https://github.com/probe-rs/flash-algorithm-template) for writing an algorithm. Please follow the instructions in the `README.md` in that repo.

## Acknowledgements

In early stages of this library, we profited invaluably from the pyOCD code to understand how flashing works. Also it's always a good reference to cross check how ARM specific things work. So, a big thank you to the team behind pyOCD!

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT) at your option.

## Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
