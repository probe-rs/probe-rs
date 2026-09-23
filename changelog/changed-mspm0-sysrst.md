Change MSPM0 reset process to perform SYSRST instead of CPURST. This allows debugger reset to stop the watchdog which can trigger during programming.
