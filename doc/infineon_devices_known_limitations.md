# Known limitations for Infineon Devices

1. Programming of Bank 2 in dual-bank flash is not supported on dual-bank PSOC C3 and PSOC C3 x7/x8 devices.
2. Memory maps for PSOC Edge E84 do not reflect configurations utilizing reclaimed RRAM.
3. The `read` and `write` commands do not enforce validation of target address ranges, permitting access to addresses outside defined memory regions. This limitation applies to all targets.
4. The PSOC Edge E84 exclusively supports the default external 16Mb QSPI flash; alternative or secondary QSPI memory configurations are not supported at this time.  
5. Custom external memory on the KIT_PSE84_HMI device is not supported. Please refrain from using full device erase operations, including the `erase` command or the `--chip-erase` subcommand, as these actions may result in unintended consequences.
6. JTAG-based debugging is not available on PSOC Edge E84 and PSOC C3 devices; only SWD is supported.
7. J-Link OB (on-board) probes do not support SWD speeds above 3000 kHz; use `--speed 3000` or lower.
