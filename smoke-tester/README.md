# Smoke Testing Tool


## Quick testing with a single chip

To test a single board quickly, the chip and probe can be specified directly on the command line:

```console
cargo run -- test --chip nrf51822_xxAB --probe 0d28:0204
```

## Multiple boards

To regularly test multiple boards, it is recommended to create a folder containing the necessary setup data for these boards. 
In this folder, create a .toml file for each board, containing the following information:


```toml
# Test description for Microbit v1

chip = "nrf51822_xxAB"

probe_selector = "0d28:0204:9900360150494e4500492002000000600000000097969901"

# Optional binary (ELF format), used to test flashing. Needs to be specified as an absolute path.
flash_test_binary = "gpio_hal_blinky"
```

Specifying a binary for flashing is optional.

To turn the USB port power of the probe off and on before each test, add:

```toml
cycle_power = true
```

This works only on Linux. The user needs write access to the `disable` file of the USB port in sysfs.

The smoke tester can called like this:

```console
cargo run -- --dut-definitions <dut_dir>
```

## Benchmarks

The `benchmark_memory` test measures memory access latency and throughput. The flashing test measures the throughput of each flash operation. To record the results, set `SMOKE_TESTER_BENCH_OUT` to a file path. The tests append the results to the file as JSON lines. Without the variable, the benchmark runs each operation once as a test.

```console
mkdir -p bench/bench-local
SMOKE_TESTER_CONFIG=<dut_dir> SMOKE_TESTER_BENCH_OUT=bench/bench-local/bench.jsonl cargo nextest run --profile ci-hardware
python3 smoke-tester/bench_report.py bench report
```

`bench_report.py` writes `report/summary.md`. With `--baseline <file> --baseline-sha <sha>`, it also compares the results. In CI, the results of each master commit are in the `bench-data` branch, and a PR comment shows the changes.
