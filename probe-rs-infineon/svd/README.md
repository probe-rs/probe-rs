# AURIX SVD files

Infineon CMSIS-SVD files ("PD" variants) for the AURIX device families. `src/svd.rs` matches a
chip name to one of them (for example `TC275` → `TC27XDPD`); the probe-rs debugger uses it for
the peripheral view when a launch configuration names no `svdFile`.

- Source: [ZeZhan/vscode-aurix-tcf-debugger](https://github.com/ZeZhan/vscode-aurix-tcf-debugger)
  `bin/svd/`, commit `fb83b4eb92bbf84c43c6d63399221336d2052877`, unchanged (gzip compressed).
- License: each file carries the Infineon license text, which allows distribution within
  development tools that support Infineon microcontrollers:

  > Copyright (C) 2024 Infineon Technologies AG. All rights reserved.
  >
  > Infineon Technologies AG (Infineon) is supplying this software for use with Infineon's
  > microcontrollers. This file can be freely distributed within development tools that are
  > supporting such microcontrollers.
  >
  > THIS SOFTWARE IS PROVIDED "AS IS". NO WARRANTIES, WHETHER EXPRESS, IMPLIED OR STATUTORY,
  > INCLUDING, BUT NOT LIMITED TO, IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
  > PARTICULAR PURPOSE APPLY TO THIS SOFTWARE. INFINEON SHALL NOT, IN ANY CIRCUMSTANCES, BE LIABLE
  > FOR SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, FOR ANY REASON WHATSOEVER.

Notes:

- `TC27XDPD` describes the TC27x D-step. The C-step (JTAG ID `0x401DA083`) has the same
  peripheral addresses for the registers checked on a TC275 C-step ED (CPU0–2 CSFR: DBGSR, PC).
- The SCU of `TC27XDPD` lists only the clock registers (no CHIPID or watchdog registers).
- The CPU peripherals contain `TRIG_ACC`, which clears on read. The debugger shows peripherals
  only while the core is halted, after it has taken the halt reason from that register.
- A plain `TC27XDPD.svd` in a directory searched first (the launch `cwd`'s `.vscode/svd` or
  `svd`, or `PROBE_RS_SVD_DIR`) replaces the bundled file.
