# Notes about tests

Until we have a better mechanism in place, the test are run against binaries which are prebuilt on some local machine.

The source code for the tests can be found at locations below. Please note that if these binaries are re-built, it is likely that memory locations in tests such as `./source_location.rs` will have to be updated to match the new binaries.

- `inlined-functions`, `exceptions`
  <https://github.com/Tiwalun/probe-rs-repro.git>, commit 5fc1b7784d66e45aa2488a56130abe6be0eed695, using the `build_all.sh` script.
- `probe-rs-debugger-tests`
  - This binary was created using the `STM32H745ZITx` feature of the [probe-rs-debugger testing application](https://github.com/probe-rs/probe-rs-debugger-test). Clone the above repository, and then follow these steps to recreate the binary:

    ```bash
    git checkout 14bbaf86d5042f25ee8bce0ac8b1dea0c06adb4a
    cargo build --target thumbv7em-none-eabihf --features STM32H745ZITx --locked
    ```

- `debug-unwind-tests`
  - This binary was created using the various chip specific binaries of the [probe-rs-debugger testing application](https://github.com/probe-rs/probe-rs-debugger-test).
    - To reproduce the coredump and elf files, clone commit `8a02600045eef3daf80e1976e8db67c565bf8931` of the above repository, and then follow the steps in the `README.md` file in the root of that repository.
    - In the case of tests failing, use [cargo insta review](https://insta.rs/docs/quickstart/) to easily compare changes.
