# probe-rs
<a href="https://travis-ci.com/Yatekii/probe-rs">
    <img src="https://travis-ci.com/Yatekii/probe-rs.svg?branch=master" alt="build:passed">
</a>

A debugging toolset and library for debugging ARM cores on a separate host.

## Motivation

The goal of this library is to provide a toolset to interact with a variety of embedded MCUs and debug probes.
For starters, ARM cores will be supported with use of the CoreSight protocol.
If there is high demand and more contributors, it is intended to add support for other architectures.

Similar projects like OpenOCD, PyOCD, Segger Toolset, ST Tooling, etc. exist.
They all implement the GDB protocol and their own protocol on top of it to enable GDB to commuicate with the debug probe.
This is not standardized and also little bit unstable sometimes. For every tool the commands are different and so on.

This project gets rid of the GDB layer and provides a direct interface to the debug probe,
which then enables other software, for example [VisualStudio](https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website) to use it's debug functionality.

What's more is that we can use CoreSight to its full extent. We can trace and modify memory as well as registers in real time.

*The end goal is a complete library toolset to enable other tools to use the functionality of CoreSight.*

## Functionality

The lib can connect to a DAPLink and read and write memory correctly.
It can read ROM tables and extract CoreSight component information.
Writing an entire hex file is halfaways there.
The lib can also connect to an [ST-Link](https://www.st.com/en/development-tools/st-link-v2.html), attach to an STM32F429 (it should be able to connect to any target; this one was just used for testing) and read DAP registers. Reading ROM tables is buggy because of some STLink troubles but should possibly fixed in the long run.

Focus of the development is having a full implementation (CoreSight, Flashing, Debugging) working for the DAPLink and go from there.

### CLI

To demonstrate the functionality a small cli was written.
Fire it up with

```
cargo run -p cli -- help
```

The help dialog should then tell you how to use the CLI.

For using the tracer fire

```
cargo run -p cli -- trace <n> <address> | python3 cli/update_plot.py
```

The pipe interface is binary for now.

Here is how it looks if you do everything correct and you trace a memory location with a changing value:

<p align="center">
    <img src="https://github.com/Yatekii/probe-rs/blob/master/doc/img/counter.png" alt="counter plot">
</p>

## FAQ

### I need help!

Don't hesitate to [file an issue](https://github.com/Yatekii/probe-rs/issues/new), ask questions on [irc](irc://irc.mozilla.com#rust-embedded), or contact [@Yatekii](https://github.com/Yatekii) by e-mail.

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

### Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.