# WS63 flash algorithm

The flash-algorithm loader source has moved out of this fork into its own
repository:

**https://github.com/hispark-rs/hisi-flash-algorithm** (the `ws63` crate)

It is a standalone `#![no_std]` `riscv32imc` blob (the WS63 SFC v150 NOR flash
loader) that drives WREN / RDSR / 4K sector-erase / page-program and clears the
flash chip's block-protect bits on Init. It builds on **stable rust** with the
standard rustup target and is **not** a member of this workspace — it is built
separately, extracted with `target-gen`, and embedded into the target YAML as
base64.

The compiled algorithm is already embedded in
[`probe-rs/targets/HiSilicon_WS63.yaml`](../../probe-rs/targets/HiSilicon_WS63.yaml)
(`flash_algorithms: [ws63-sfc]`); `probe-rs download` erase/program is
hardware-verified on a real WS63 board (GD25Q32). Nothing here needs to change to
use it.

## Re-generating / updating the embedded algorithm

If the loader changes upstream, rebuild it and splice the new ELF back into the
WS63 target:

```bash
# in a checkout of https://github.com/hispark-rs/hisi-flash-algorithm:
rustup target add riscv32imc-unknown-none-elf
cargo build --release -p ws63-flash-algorithm
# -> target/riscv32imc-unknown-none-elf/release/ws63-flash-algorithm

# then, pointing target-gen at this probe-rs checkout (<probe-rs>):
cargo run -p target-gen --manifest-path <probe-rs>/Cargo.toml -- \
  elf target/riscv32imc-unknown-none-elf/release/ws63-flash-algorithm \
  -n ws63-sfc \
  --update <probe-rs>/probe-rs/targets/HiSilicon_WS63.yaml
```

`target-gen --update` refills `instructions` (base64), the `pc_*` routine entry
points, `data_section_offset`, and `flash_properties` for the `ws63-sfc` block.
