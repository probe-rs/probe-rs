Changed how architecture-specific core interfaces are accessed:
 - `probe::Probe::try_get_xtensa_interface` now takes a `XtensaState` object that should be saved
   between interface accesses.
 - `probe::Probe::try_get_riscv_interface` now returns a builder object to create state objects, and
   to attach the probe using such a state object.