# probe-rs
<a href="https://travis-ci.org/Yatekii/probe-rs">
    <img src="https://img.shields.io/travis/Yatekii/probe-rs/master.svg" alt="Travis Build Status">
</a>

A debugging toolset and library for debugging ARM cores on a separate host

## Motivation

The goal of this library is to provide a toolset to interact with a variety of embedded MCUs and debug probes.
For starters, ARM cores will be supported with use of the CoreSight protocol.
If there is high demand and more contributors, it is intended to add support for other architectures.

Similar projects like OpenOCD, PyOCD, Segger Toolset, ST Tooling, etc. exist.
They all implement the GDB protocol and their own protocol on top of it to enable GDB to commuicate with the debug probe.
This is not standardized and also little bit unstable sometimes. For every tool the commands are different and so on.

This project leaves away the GDB layer and provides a direct interface to the debug probe,
which then enables other software, for example [https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website](VisualStudio) to use it's debug functionality.

What's more is that we can use CoreSight to its full extent. We can trace and modify memory as wella s registers in real time.

*The end goal is a complete commandline toolset REPL that can use the full functionality of CoreSight.*

## FAQ

### I need help!

Don't hesitate to [file an issue](https://github.com/Yatekii/probe-rs/issues/new), ask questions on [irc](irc://irc.mozilla.com#rust-embedded), or contact [@Yatekii](https://github.com/Yatekii) by e-mail.

### How can I help?

See [CONTRIBUTING.md](https://github.com/Yatekii/probe-rs/blob/master/CONTRIBUTING.md).

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
