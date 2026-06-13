# Adding HiSilicon WS63 (Hi3863) and BS21/BS2X (Hi2821) to probe-rs

Status: **WIP.** Items 1–3 have **landed**: the mem-AP DTM `dm_base` change, the
**WS63 built-in target** (`probe-rs/targets/HiSilicon_WS63.yaml`, debug-only), and
the **HiSilicon vendor `DebugSequence`** (`probe-rs/src/vendor/hisilicon/`) that
hooks the ARM-DAP debug-port enable. This wires the full software attach path
(DTM + target + DAP bring-up); it is still **not** validated on silicon. Attach
additionally depends on the external **GPIO_04-high-at-power-on strap** (which
probe-rs cannot perform) and on AP0 enumerating as expected. The flash algorithm
(item 4) is done and embedded — `probe-rs download` erase/program is
hardware-verified. All remaining gates are hardware. BS21 stays a scaffold until
its DM base is confirmed. See "probe-rs changes — status" and "Open items".

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

## probe-rs changes — status

Two of the four are landed; the remaining two (plus on-silicon validation) are
what stand between "wired" and "attach actually succeeds on a board".

1. ✅ **DONE — DM-base offset on the mem-AP DTM** (the core change).
   `MemApDtm::dmi_register_to_ap_address` was `dmi_register * 4`, assuming the DM
   sits at AP offset 0 (true for RP2350). WS63's DM is at `0x80000000`.
   - `probe-rs-target` `RiscvCoreAccessOptions`: added `dm_base: u64`
     (`#[serde(default)]` → 0; only meaningful with `mem_ap`).
   - `MemApDtm::new(memory, dm_base)`; address = `dm_base + dmi_register * 4`.
   - threaded `dm_base` from `core_access_options` through `riscv_mem_ap_cores`
     (now a 3-tuple) at all three `MemApDtm::new` sites in `session.rs`.
   Default 0 keeps RP235x and every existing target byte-identical.

2. ✅ **DONE (WS63) — target definition** `probe-rs/targets/HiSilicon_WS63.yaml`:
   RISC-V core `!Riscv { hart_id: 0, mem_ap: !v1 0, dm_base: 0x80000000 }` +
   memory map, plus the embedded `ws63-sfc` flash algorithm (item 4). Passes the
   `validate_builtin` test. **BS21 stays a scaffold** (`BS21.wip.yaml`) until its
   DM base + AP index are confirmed on hardware.

3. ✅ **DONE — vendor debug sequence** `probe-rs/src/vendor/hisilicon/`
   (`HiSilicon` vendor + `Ws63` sequence; registered in `vendor/mod.rs`).
   - Because WS63 is `ArmWithRiscv`, the chip bring-up must hook the **ARM** DAP
     (a RISC-V `on_connect` runs too late — after `enter_debug_mode` first touches
     the DM; and for `ArmWithRiscv` targets the DAP path uses the Arm sequence).
     So the vendor returns `DebugSequence::Arm(Ws63)`.
   - `Ws63::debug_device_unlock` performs the debug-port enable
     (`0x40010260 = 1`, "enable coresight-swd mode") as a **best-effort** write:
     if it fails it logs and continues, because on most boards the port is already
     enabled by the external **GPIO_04-high-at-power-on** strap (which probe-rs
     cannot do).
   - End-to-end wiring is covered by the `ws63_uses_hisilicon_arm_sequence` test.
   - `expose_csrs` for the LOCI CSRs has **no probe-rs equivalent** (register
     lists are compile-time `static`; a sequence can read/write any 12-bit CSR but
     can't add them to the debugger's register view) → **N/A**. `prefer_sba` is
     **moot** on the mem-AP path: DMI/memory access for a mem-AP RISC-V core always
     routes through the ARM AP regardless, so there is nothing to toggle.
   - Still **unvalidated on silicon** (needs GPIO_04 strap + AP0 to enumerate).

4. ✅ **LANDED — flash algorithm (embedded + hardware-verified).** The SFC v150
   register/command loader (WREN/RDSR/4K-erase/page-program + Init clears
   block-protect), ported from `fbb_ws63` `hal_sfc_v150` + OpenOCD `ws63.c`, is
   **embedded** into `HiSilicon_WS63.yaml` (`flash_algorithms: [ws63-sfc]`).
   `probe-rs download` erase/program is verified on a real board (GD25Q32). The
   loader source now lives in its own repo —
   **https://github.com/hispark-rs/hisi-flash-algorithm** (the `ws63` crate) — and
   is extracted/embedded with `target-gen ... --update`. See
   [`flash-algorithm.md`](./flash-algorithm.md) for the rebuild/re-embed workflow.

## Open items (need silicon)

- [ ] Confirm BS21 DM base + AP index on hardware (dump the CoreSight ROM table).
- [ ] Verify WS63 AP0 / DM `0x80000000` enumerates via probe-rs's ARM backend
      (DAP IDCODE `0x5ba0_0477`).
- [ ] Confirm the debug-enable sequence reaches a halted core from cold boot.
- [x] Author + validate the flash algorithm against a board. **Done** — embedded
      as `ws63-sfc`; source at https://github.com/hispark-rs/hisi-flash-algorithm.
- [ ] Validate against a known probe (CMSIS-DAP or FTDI FT2232H — the adapters
      HiSpark uses).

## References

- HiSpark Studio (vendor, open): `vscode-hispark-studio` —
  `src/opensource/openocd/{src/target/riscv/riscv.c (riscvcs), batchcs.c,
  src/flash/nor/ws63.c, tcl/target/vendorhm/WS63-*.cfg}`,
  `src/plugins/.../connect/CFBB/{ws63,bs21,...}/*.JLinkScript`.
- probe-rs RP2350 path: `mem_ap_dtm.rs`, `session.rs`, `targets/RP235x.yaml`.
- fbb_ws63 / fbb_bs2x SDKs (register/memory/pin ground truth).
