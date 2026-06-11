# WS63 flash algorithm (probe-rs loader)

A probe-rs flash loader for the HiSilicon WS63 (Hi3863) on-chip SFC NOR flash. It
drives the Serial Flash Controller (SFC v150) in register/command mode to issue
standard SPI-NOR commands (WREN / RDSR / 4K sector-erase / page-program).

> **Status: UNVALIDATED on silicon.** It builds and the probe-rs toolchain
> (`target-gen`) extracts a well-formed algorithm from it, but the register
> fields, the XIP→flash address mapping, and the program/erase sequence have only
> been reverse-engineered from `fbb_ws63` `hal_sfc_v150` + HiSpark Studio's
> OpenOCD `ws63.c`. It is **deliberately NOT embedded** into
> `probe-rs/targets/HiSilicon_WS63.yaml` — running an unverified erase/program
> algorithm could corrupt flash on a real board. Embed it only after validating
> against hardware (see below).

## Why a separate, excluded crate

A flash algorithm is a `#![no_std]` blob built for the **target** ISA, not the
host. It is `exclude`d from the probe-rs workspace (root `Cargo.toml`) so host
builds ignore it. Built for **`riscv32imc-unknown-none-elf`** — a subset of the
WS63 `RV32IMFC` ISA: WS63 has **no atomics** (so not `imac`), and probe-rs does
**not** preserve FP across flash-algo calls (so no `F`).

## Build

```bash
rustup target add riscv32imc-unknown-none-elf
cd doc/hisilicon-ws63-bs21/flash-algorithm
cargo build --release
# -> target/riscv32imc-unknown-none-elf/release/ws63-flash-algorithm
```

The `flash-algorithm` crate supplies the panic handler and the `PrgCode`/`PrgData`
linker sections (`memory.x`); `.cargo/config.toml` passes `-Tmemory.x`. The ELF
exports `Init` / `UnInit` / `EraseSector` / `ProgramPage` + the `FlashDevice`
descriptor.

## Extract + integrate (after hardware validation)

```bash
# from the probe-rs repo root:
ELF=doc/hisilicon-ws63-bs21/flash-algorithm/target/riscv32imc-unknown-none-elf/release/ws63-flash-algorithm
# generate a standalone YAML block to inspect:
cargo run -p target-gen -- elf "$ELF" -n ws63-sfc /tmp/ws63_algo.yaml
# OR splice it straight into the WS63 target:
cargo run -p target-gen -- elf "$ELF" -n ws63-sfc --update probe-rs/targets/HiSilicon_WS63.yaml
```

Then set the variant's `flash_algorithms: [ws63-sfc]` in `HiSilicon_WS63.yaml`
(currently `[]`). `target-gen` fills `instructions` (base64), `pc_init`,
`pc_uninit`, `pc_program_page`, `pc_erase_sector`, `data_section_offset`, and
`flash_properties` (range `0x200000..0xa00000`, page `0x100`, 4K sectors).

## What it does (SFC v150 register/command mode)

- SFC base `0x4800_0000`: `cmd_config@0x300`, `cmd_ins@0x308`, `cmd_addr@0x30c`,
  `cmd_databuf[0..15]@0x400` (64 B max per reg-mode transfer).
- `erase_sector`: WREN → opcode `0x20` (4K) at the flash offset → poll RDSR WIP.
- `program_page`: for each ≤64 B chunk → WREN → load `cmd_databuf` → opcode `0x02`
  → poll RDSR WIP.
- CPU XIP address → flash offset via `XIP_BASE = 0x200000`.

## Validation checklist (needs a board)

- [ ] Confirm `sel_cs` (the port writes 1) is correct for the WS63 flash CS.
- [ ] Confirm the XIP→flash offset mapping (`addr - 0x200000`).
- [ ] Confirm reg-mode commands work while/instead-of XIP bus mode (may need to
      disable bus mode first).
- [ ] Confirm 3-byte addressing (`global_config.flash_addr_mode == 0`). Correct
      for ≤16 MiB flash and the SDK default, but the port does not set it — if the
      SFC is left in 4-byte mode, addresses would be wrong.
- [ ] Confirm `cmd_config` bit positions against live silicon.
- [ ] Tune `program_page_timeout` / `erase_sector_timeout`.
- [ ] End-to-end `probe-rs download` + verify against a known image.
