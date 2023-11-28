Added support for riscv semihosting SYS_EXIT syscall.

Please note that probe-rs with espflash < 3.0 will probably fail to flash a binary containing semihosting calls due to https://github.com/esp-rs/espflash/issues/522