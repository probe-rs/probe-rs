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
