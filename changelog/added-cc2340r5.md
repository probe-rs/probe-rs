Added support for TI's CC2340R5.

- `auto_attach` (and thus `target-gen`) doesn't work with this chip yet due to
a bug where the device responds OK with `DAP_Connect` as Jtag which is
not supported by the device but is the default interface on the XDS110.
