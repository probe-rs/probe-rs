Added support for riscv semihosting SYS_EXIT syscall.

Please note that probe-rs will probably fail to flash a binary containing semihosting calls, when using esp-hal-common <= 0.14.1, due to https://github.com/esp-rs/espflash/issues/522