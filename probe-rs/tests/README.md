# Notes about tests

Until we have a better mechanism in place, the test are run against binaries which are prebuilt on some local machine.

The source code for the tests can be found at locations below. Please note that if these binaries are re-built, it is likely that memory locations in tests such as `./source_location.rs` will have to be updated to match the new binaries.
- `inlined_function` 
  - The source for this binary is unknown. //TODO: Consider re-writing tests against source code in `probe-rs-debugger-test`, and removing the `inlined_function` binary from this repo.
- `probe-rs-debugger-tests`
  - This binary was created using the `STM32H745ZITx` feature of the [probe-rs-debugger testing application](https://github.com/probe-rs/probe-rs-debugger-test)
  
