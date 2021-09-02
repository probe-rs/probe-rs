# Snapshot Tests go 📸✨

All tests in this directory are snapshot tests, e.g. they compare `probe-run` output to a previous, known-good state.

These tests need to be run *manually* because they require the target hardware to be present.

To do this,
1. connect a nrf52840 DK to your computer via the J2 USB port on the *short* side of the DK
2. run `cargo test -- --ignored`

## adding a new snapshot test

### 1. compile a suitable ELF file
By default, your elf file should be compiled to run on the `nRF52840_xxAA` chip.
You can e.g. check that your `.cargo/config.toml` is set to cross-compile to the `thumbv7em-none-eabihf` target:

```toml
[build]
# cross-compile to this target
target = "thumbv7em-none-eabihf" # = ARM Cortex-M4
```

🔎 You can retrieve your ELF file from the `target/thumbv7em-none-eabihf/` folder of your app.

```console
$ # get ELF `hello` that was compiled in debug mode
$ cd target/thumbv7em-none-eabihf/debug/
$ # make sure that it's an elf file
$ file `hello`
hello: ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, with debug_info, not stripped
$ # copy it into `probe-run/tests/test_elfs`
$ cp hello my/path/to/probe-run/tests/test_elfs
```
❗️ if you'd rather not have full paths containing your name, folder structure etc. show up in your backtrace, extend your `.cargo/config.toml` like so:

```diff
# either in your [target.xxx] or [build] settings
rustflags = [
    ...
+      "--remap-path-prefix", "/Users/top/secret/path/=test_elfs",
]
```

### 2. write test and run it once

Write your test that captures `probe-run`s output for your test ELF and check the result with `insta::assert_snapshot!(run_output);`

### 3. cargo insta review
When you run `cargo test -- --ignored` for the first time after you've added your new test, it will fail.
This first run creates a snapshot which you can then store as a "known good"

run
```console
$ cargo install cargo-insta
$ cargo insta review
```

And review the snapshot that was created. Accept it if it looks right to you (adjust test, re-run and review again if it doesn't).

Now, your test will fail in the future if the output doesn't match the snashot you created.

For details, refer to the [insta](https://docs.rs/insta/1.7.1/insta/#writing-tests) docs.
