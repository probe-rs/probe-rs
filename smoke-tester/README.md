# Smoke Testing Tool


## Quick testing with a single chip

To test a single board quickly, the chip and probe can be specified directly on the command line:

```console
cargo run -- --chip nrf51822_xxAB --probe 0d28:0204
```

## Multiple boards

To regularly test multiple boards, it is recommended to create a folder containing the necessary setup data for these boards. 
In this folder, create a .toml file for each board, containing the following information:


```toml
# Test description for Microbit v1

chip = "nrf51822_xxAB"

probe_selector = "0d28:0204:9900360150494e4500492002000000600000000097969901"

# Optional binary (ELF format), used to test flashing.
# The path is relative to the .toml file.
flash_test_binary = "gpio_hal_blinky"
```

Specifying a binary for flashing is optional.

The smoke tester can called like this:

```console
cargo run -- --dut-definitions <dut_dir>
```

