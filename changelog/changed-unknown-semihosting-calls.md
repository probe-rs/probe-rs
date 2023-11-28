Continue running the core if an unsupported semihosting call occured.

Previously, `probe-rs run` would exit with "the CPU halted unexpectedly", whenever an unknown semihosting operation occurred.
With this change, `probe-rs run` will print a warning and then automatically continue the core.