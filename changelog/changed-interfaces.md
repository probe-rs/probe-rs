Changed how architecture-specific core interfaces are accessed:
 - `probe::Probe::try_get_xtensa_interface` now takes an `XtensaDebugInterfaceState` object that should be saved and reused between interface accesses.
 - `probe::Probe::try_get_riscv_interface` has been renamed to `try_get_riscv_interface_factory` and returns a builder object to create state objects, and to attach the probe using such a state object.
 - The corresponding `DebugProbe` APIs have been changed.