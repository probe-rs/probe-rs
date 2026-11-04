Fixed `probe-rs debug` panicking when the process has no controlling terminal (CI, cron, ssh without `-t`); it now exits with an error that names `probe-rs gdb` and `probe-rs dap-server`.
