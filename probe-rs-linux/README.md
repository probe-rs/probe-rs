# Linux probe drivers for probe-rs

This crate contains Linux-specific probe drivers for probe-rs:

- `linuxgpiod` — bit-bangs SWD over the Linux GPIO character-device
  interface (`/dev/gpiochipN`).

Drivers are no-ops on non-Linux targets so the crate always compiles.

## Usage

Add the crate as a dependency in your `Cargo.toml`:

```toml
[dependencies]
probe-rs-linux = <current version>
```

Then register the plugin with probe-rs:

```rust
fn main() {
    probe_rs_linux::register_plugin();

    // ... rest of the code
}
```

## linuxgpiod (bit-banged SWD over GPIO)

Select a probe with the synthetic selector
`0:0:<gpiochip>,swclk=<offset>,swdio=<offset>[,srst=<offset>]`, where
`<gpiochip>` is `gpiochipN`, `/dev/gpiochipN`, or just `N`:

```bash
probe-rs run --probe 0:0:gpiochip1,swclk=26,swdio=25,srst=38 \
    --chip STM32F439ZITx hello_world.elf
```

The VID:PID portion of the selector is ignored; the serial portion carries
the chip-and-pin map. `srst` is optional.
