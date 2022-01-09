# probe-rs-debugger

A debugger that uses the [probe-rs](https://github.com/probe-rs/probe-rs) library to provide an interactive debugging experience.

## Installation

```
cargo install --git https://github.com/probe-rs/probe-rs probe-rs-debugger
```

## Usage

Assuming that `CARGO_HOME` is in your path, you can try any of the following:

1. For full list of command line options

```
probe-rs-debugger --help
``` 

2. An fully qualified example of a CLI based debug session

```
probe-rs-debugger debug --chip STM32H745ZITx --speed 24000 --probe PID:VID --program-binary ./target/thumbv7em-none-eabihf/debug/debug_example --protocol swd --connect-under-reset  --core-index 0 --flashing-enabled --reset-after-flashing --halt-after-reset
```

3. Starting a DAP server on a specific port, to allow a DAP Client like VSCode to connect 

```
probe-rs-debugger debug --dap --port 50001
```

## Additional information

**Please refer to [probe-rs/vscode](https://github.com/probe-rs/vscode) for additional instructions, as well as up to date information on supported functionality and ongoing improvements.**

## Acknowledgements

This debugger is an extension of, and builds on the prior work of the [probe-rs](https://github.com/probe-rs) community.

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

