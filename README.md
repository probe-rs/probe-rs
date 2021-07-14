# probe-rs

a modern, embedded debugging toolkit,
written in Rust

[![crates.io](https://meritbadge.herokuapp.com/probe-rs)](https://crates.io/crates/probe-rs) [![documentation](https://docs.rs/probe-rs/badge.svg)](https://docs.rs/probe-rs) [![Actions Status](https://github.com/probe-rs/probe-rs/workflows/CI/badge.svg)](https://github.com/probe-rs/probe-rs/actions) [![chat](https://img.shields.io/badge/chat-probe--rs%3Amatrix.org-brightgreen)](https://matrix.to/#/#probe-rs:matrix.org) 

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

## Downloading a file

The `cargo-flash` utility can be used as a cargo subcommand to download a compiled Rust program onto a target device. It can also be used to download arbitrary ELF files that might come out of a C/C++ compiler. Have a look at [cargo-flash](https://github.com/probe-rs/cargo-flash) for more information.

## Better debugging with probe-rs

If you are looking for a more extended debugging experience, please head over to [cargo-embed](https://github.com/probe-rs/cargo-embed) which provides support for GDB, RTT, and config files.

## VSCode

We are implementing [Microsoft DAP](https://microsoft.github.io/debug-adapter-protocol/). This makes embedded debugging via probe-rs available in modern code editors implementing the standard, such as VSCode. To support this, probe-rs includes a debugger which supports both basic command line debugging, and more extensive capabilities when run as a DAP server. Please see [probe-rs-debugger](https://github.com/probe-rs/probe-rs/tree/dap/debugger) and [vscode](https://github.com/probe-rs/vscode) for more information.

## Usage Examples
### Halting the attached chip

```rust
use probe_rs::Probe;

fn main() -> Result<(), probe_rs::Error> {
    // Get a list of all available debug probes.
    let probes = Probe::list_all();

    // Use the first probe found.
    let probe = probes[0].open()?;

    // Attach to a chip.
    let mut session = probe.attach("nrf52")?;

    // Select a core.
    let mut core = session.core(0)?;

    // Halt the attached core.
    core.halt(std::time::Duration::from_millis(300))?;

    Ok(())
}
```

### Reading from RAM

```rust
use probe_rs::{MemoryInterface, Session};

fn main() -> Result<(), probe_rs::Error> {
    // Attach to a chip.
    let mut session = Session::auto_attach("nrf52")?;

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

### How can I help?

Please have a look at the issues or open one if you feel that something is needed.

Any contributions are very welcome!

Also have a look at [CONTRIBUTING.md](CONTRIBUTING.md).

### Our company needs feature X and would pay for its development

Please reach out to [@Yatekii](https://github.com/Yatekii)

### Building

Building requires Rust and Cargo which can be installed [using rustup](https://rustup.rs/). probe-rs also depends on libusb and libftdi. On linux these can be installed with your package manager:

```console
# Ubuntu
> sudo apt install -y libusb-1.0-0-dev libftdi1-dev libudev-dev

# Fedora
> sudo dnf install -y libusbx-devel libftdi-devel libudev-devel
```

On Windows you can use [vcpkg](https://github.com/microsoft/vcpkg#quick-start-windows):

```console
# dynamic linking 64-bit
> vcpkg install libftdi1:x64-windows libusb:x64-windows
> set VCPKGRS_DYNAMIC=1

# static linking 64-bit
> vcpkg install libftdi1:x64-windows-static-md libusb:x64-windows-static-md
```

See [the vcpkg crate documentation](https://docs.rs/vcpkg/) for more information about configuring vcpkg with rust.

### Adding Targets

Target files are generated using [probe-rs/target-gen](https://github.com/probe-rs/target-gen) from CMSIS packs provided [here](https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search).
Generated files are then placed in `probe-rs/targets` for inclusion in the probe-rs project.

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
