---
name: Bug report
about: Create a report to help us improve
title: ''
labels: bug
assignees: ''

---

**Describe the bug**
A clear and concise description of what the bug is.

**To Reproduce**
Steps to reproduce the behavior:

*Example*
1. Write `src/bin/abort.rs`
``` rust
// ..
fn main() -> ! {
    cortex_m::asm::udf()
}
```
2. Run it with `cargo run --bin abort`

**Expected and observed behavior**
A clear and concise description of what you expected to happen. Please include relevant console output.

*Example*: I expected to see a backtrace but instead the `cargo run` command hanged / stopped responding.
``` console
$ cargo run --bin hello
    Finished dev [optimized + debuginfo] target(s) in 0.03s
     Running `probe-run --chip nrf52840 --defmt target/thumbv7em-none-eabihf/debug/hello`
  (HOST) INFO  flashing program
  (HOST) INFO  success!
────────────────────────────────────────────────────────────────────────────────
stack backtrace:
(.. probe-run hangs here ..)
```

**config.toml**
The contents of your project's `.cargo/config.toml` file

*Example*:

``` toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-run --chip nrf52840 --defmt"
rustflags = [
  "-C", "linker=flip-link",
  "-C", "link-arg=-Tlink.x",
  "-C", "link-arg=-Tdefmt.x",
]

[build]
target = "thumbv7em-none-eabihf" # Cortex-M4F and Cortex-M7F (with FPU)
```

**Probe details**
You can get this information from [`probe-rs-cli`](https://crates.io/crates/probe-rs-cli). Your microcontroller must be connected to your PC / laptop when you run the command below.

*Example:*

``` console
$ probe-rs-cli list
[0]: DAPLink CMSIS-DAP (VID: 0d28, PID: 0204, Serial: abc, DAPLink)
```

**Operating System:**
[e.g. Linux]

**ELF file (attachment)**

Please attach to this bug report the ELF file you passed to `probe-run`. The path to this file will appear in the output of `cargo run`. If you'd prefer not to upload this file please keep it around as we may ask to run commands like [`nm` or `objdump`](https://crates.io/crates/cargo-binutils) on it.

*Example*:

``` console
$ cargo run
    Running `probe-run --chip nrf52840 --defmt target/thumbv7em-none-eabihf/debug/hello`
```

Attach the file `target/thumbv7em-none-eabihf/debug/hello` to the bug report.

**Additional context**
Add any other context about the problem here.
