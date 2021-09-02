# probe-rs-rtt

[![crates.io](https://img.shields.io/crates/v/probe-rs-rtt)](https://crates.io/crates/probe-rs-rtt) [![documentation](https://docs.rs/probe-rs-rtt/badge.svg)](https://docs.rs/probe-rs-rtt)

Host side implementation of the RTT (Real-Time Transfer) I/O protocol over probe-rs.

## [Documentation](https://docs.rs/probe-rs-rtt)

RTT implements input and output to/from a microcontroller using in-memory ring buffers and memory polling. This enables debug logging from the microcontroller with minimal delays and no blocking, making it usable even in real-time applications where e.g. semihosting delays cannot be tolerated.

This crate enables you to read and write via RTT channels. It's also used as a building-block for probe-rs debugging tools.
