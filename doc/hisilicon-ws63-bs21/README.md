# Adding HiSilicon WS63 (Hi3863) and BS21/BS2X (Hi2821) to probe-rs

Status: **WIP.** The core enabler — the mem-AP DTM `dm_base` change (item 1 below)
— has **landed**, and **WS63 is now a built-in target** (`probe-rs/targets/
HiSilicon_WS63.yaml`, debug-only). Debug bring-up (attach/halt/memory/registers)
is wired; flash programming and on-silicon validation remain open and need
hardware (see "Open items"). BS21 stays a scaffold until its DM base is confirmed.

## The chips

| probe-rs target | Vendor part | Core | Connectivity | Notes |
|---|---|---|---|---|
| WS63 | Hi3863 | HiSilicon RISC-V "riscv31" (RV32IMFC_Zicsr + custom) @240 MHz | Wi-Fi 6 + BLE + SLE | 8 MB XIP NOR flash |
| BS21 / BS2X | Hi2821 | same family, "linx131" @64 MHz | BLE 5.4 + SLE (no Wi-Fi) | BS20/BS21E/BS22 SKUs |

Both run a HiSilicon RISC-V core. Crucially, the **RISC-V Debug Module is reached
through an ARM CoreSight DAP** (an AHB-AP), *not* over a native RISC-V JTAG-DTM.

## Why this is feasible without new core architecture

probe-rs already implements exactly this topology for the **RP2350** (Hazard3
RISC-V behind a CoreSight mem-AP):

- `probe-rs/src/architecture/riscv/dtm/mem_ap_dtm.rs` — a `DtmAccess` impl that
  performs RISC-V DMI reads/writes as **memory accesses through an ARM mem-AP**.
- `probe-rs/src/session.rs` (~L389) — any target core whose `core_type` is RISC-V
  **and** that declares a `memory_ap()` is automatically driven via `MemApDtm`
  through the ARM CoreSight interface (`ArchitectureInterface::ArmWithRiscv`).
- Target opt-in is pure YAML: `core_access_options: !Riscv { mem_ap: ... }`
  (see `probe-rs/targets/RP235x.yaml`, the `RP235x_riscv` variant).

So the HiSilicon "riscvcs" capability that required a dedicated **OpenOCD patch**
(`riscvcs` target type + `batchcs.c`, shipped in HiSpark Studio's bundled OpenOCD)
is **already upstream in probe-rs**. The work here is a target definition + a small
DM-base extension + a debug sequence + a flash algorithm — not a new backend.

## Ground truth (reverse-engineered)

From HiSpark Studio's bundled, patched OpenOCD (`tcl/target/vendorhm/WS63-*.cfg`,
`src/flash/nor/ws63.c`) and the fbb_ws63 / fbb_bs2x SDKs:

### WS63 (Hi3863)
- CoreSight DAP IDCODE: `0x5ba00477` (JTAG) / `0x5ba02477` (SWD) — standard ARM DAP.
- RISC-V DM behind **AP 0** (OpenOCD `-apsel 0`), **DM base `0x80000000`**
  (OpenOCD `-dbgbase 0x80000000`).
- Custom CSRs to expose: `932-943, 1984-1999, 2008` (decimal) = the LOCI/HiSilicon
  CSRs (`0x3A4-0x3AF, 0x7C0-0x7CF, 0x7D8`).
- `riscv set_prefer_sba off` (no system-bus access; use abstract commands / progbuf).
- Memory map (from `hisi-riscv-rt/memory.x`):
  - BOOTROM `0x100000` (36 K), ROM `0x109000` (268 K)
  - ITCM `0x14C000` (16 K), DTCM `0x180000` (16 K)
  - FLASH (XIP SPI NOR, 8 MB) `0x200000`; app/PROGRAM region `0x230300`
  - SRAM (L2) `0xA00000` (576 K)
- Flash: OpenOCD bank `ws63 0x230300 0x10000` + `ws63_info` driver
  (`src/flash/nor/ws63.c`; INFO flash page `0x100`, `0x4000` pages).
- Debug-enable: the debug pads are muxed by default — **GPIO_04 must be high at
  power-on** to switch them to the debug interface (WS63 HW guide / ws63-guide ch7).

### BS21 (Hi2821)
- Same CoreSight-DAP-fronted RISC-V. HiSpark ships **only a J-Link path** for BS2X
  (`connect/CFBB/bs21/connectCore.JLinkScript`): it adds 3 AHB-APs and selects
  **AHB-AP index 1** to reach the RISC-V core. **(No OpenOCD vendorhm cfg → the
  DM base for BS21 is not yet confirmed; needs hardware/ROM-table dump.)**
- SWD pins: ULP_GPIO **pin 32 = SWD_CLK, pin 33 = SWD_IO** (fbb_bs2x
  `ulp_gpio.c`); DAP autocg bypass reg `0x52000190` (`DAP_H2P_AUTOCG_BYPASS`).
- Memory map (from the bs2x QEMU machine / SDK `platform_core.h`):
  - FLASH (XIP) `0x10000000`; SRAM (L2) `0x100000` (128 K BS20 / 160 K BS21E·BS22)
  - ITCM `0x80000`, DTCM `0x20000000`

## Required probe-rs changes

1. **DM-base offset on the mem-AP DTM** (small, the only core change).
   `MemApDtm::dmi_register_to_ap_address` returns `dmi_register * 4`, i.e. it
   assumes the DM sits at AP offset 0 (true for RP2350). WS63's DM is at
   `0x80000000`. Add an optional base:
   - `probe-rs-target` `RiscvCoreAccessOptions`: add `dm_base: Option<u64>`.
   - `MemApDtm::new(memory, dm_base)`; address = `dm_base + dmi_register * 4`.
   - thread `dm_base` from `core_access_options` in `session.rs` where `MemApDtm`
     is constructed (~L144, L408, L741).
   See `mem_ap_dtm.patch.txt` in this directory for a sketch.

2. **Target definitions** (`WS63.wip.yaml`, `BS21.wip.yaml` here). Once change (1)
   lands, move into `probe-rs/targets/` and regenerate the registry. They declare
   the RISC-V core with `!Riscv { hart_id: 0, mem_ap: !v1 0, dm_base: 0x80000000 }`
   and the memory map above. `flash_algorithms` is intentionally empty (see below).

3. **Debug sequence** (`probe-rs/src/vendor/hisilicon/`, new). A `DebugSequence`
   hook to:
   - run the debug-port enable (WS63: ensure GPIO_04 path / `mww 0x40010260 1`
     equivalent; BS21: `DAP_H2P_AUTOCG_BYPASS 0x52000190`),
   - `expose_csrs` for the LOCI custom CSRs,
   - set `prefer_sba = false`.
   Register the vendor in `probe-rs/src/vendor/mod.rs`.

4. **Flash algorithm** (the hard, hardware-gated part). probe-rs flash algos are
   position-independent loader blobs (built via the `flash-algorithm` crate /
   `target-gen`). Port the SFC programming sequence from OpenOCD's
   `src/flash/nor/ws63.c` (erase/program-page against the WS63 flash controller)
   into a RISC-V flash-algorithm crate, compile, and embed (`pc_init`,
   `pc_program_page`, `pc_erase_sector`, `flash_properties`). Until then, the
   targets support **attach / halt / run / memory / register / breakpoint**
   debugging but **not** flashing.

## Open items (need silicon)

- [ ] Confirm BS21 DM base + AP index on hardware (dump the CoreSight ROM table).
- [ ] Verify WS63 AP0 / DM `0x80000000` enumerates via probe-rs's ARM backend
      (DAP IDCODE `0x5ba0_0477`).
- [ ] Confirm the debug-enable sequence reaches a halted core from cold boot.
- [ ] Author + validate the flash algorithm against a board.
- [ ] Validate against a known probe (CMSIS-DAP or FTDI FT2232H — the adapters
      HiSpark uses).

## References

- HiSpark Studio (vendor, open): `vscode-hispark-studio` —
  `src/opensource/openocd/{src/target/riscv/riscv.c (riscvcs), batchcs.c,
  src/flash/nor/ws63.c, tcl/target/vendorhm/WS63-*.cfg}`,
  `src/plugins/.../connect/CFBB/{ws63,bs21,...}/*.JLinkScript`.
- probe-rs RP2350 path: `mem_ap_dtm.rs`, `session.rs`, `targets/RP235x.yaml`.
- fbb_ws63 / fbb_bs2x SDKs (register/memory/pin ground truth).
