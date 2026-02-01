# Espressif device support for probe-rs

This crate contains code and data necessary for probe-rs to support Espressif devices.

## Usage

To use this crate, add it as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
probe-rs-espressif = <current version>
```

Then, register the plugin with probe-rs:

```rust
fn main() {
    probe_rs_espressif::register_plugin();

    // ... rest of the code
}
```
