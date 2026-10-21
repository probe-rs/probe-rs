`DebugProbe` implementations must implement `set_reset_line_connected`. If the reset line is not connected, the probe must not assert it. `ResetLine` does the checks.
