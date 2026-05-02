# probe-rs-meta

This crate allows embedding metadata into ELF binaries so that probe-rs can autodetect it.
This way you can run the tests by simply doing `probe-rs run <ELF>`, without adding
any extra flags.

## Usage

You can specify metadata, for example:

```rust
probe_rs_meta::chip!(b"rpi-pico");
probe_rs_meta::timeout!(10);
```

## Minimum supported Rust version (MSRV)

`probe-rs-meta` is guaranteed to compile on the latest stable Rust version at the time of release. It might compile with older versions but that may change in any new patch release.

## License

This work is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
