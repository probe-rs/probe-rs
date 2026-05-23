# Linux probe drivers for probe-rs

This crate contains Linux-specific probe drivers for probe-rs:

- `linuxgpiod` — bit-bangs SWD over the Linux GPIO character-device
  interface (`/dev/gpiochipN`).
- `linuxspidevswd` — emulates SWD over a `spidev` SPI bus with PICO and
  POCI tied together through a series resistor.

Both drivers are no-ops on non-Linux targets so the crate always compiles.

## Usage

Add the crate as a dependency in your `Cargo.toml`:

```toml
[dependencies]
probe-rs-linux = <current version>
```

Then register the plugin with probe-rs:

```rust
fn main() {
    probe_rs_linux::register_plugin();

    // ... rest of the code
}
```

## linuxgpiod (bit-banged SWD over GPIO)

Select a probe with the synthetic selector
`0:0:<gpiochip>,swclk=<offset>,swdio=<offset>[,srst=<offset>]`, where
`<gpiochip>` is `gpiochipN`, `/dev/gpiochipN`, or just `N`:

```bash
probe-rs run --probe 0:0:gpiochip1,swclk=26,swdio=25,srst=38 \
    --chip STM32F439ZITx hello_world.elf
```

The VID:PID portion of the selector is ignored; the serial portion carries
the chip-and-pin map. `srst` is optional.

## linuxspidevswd (SWD over spidev)

### Wiring on a Raspberry Pi

To use this backend on a Raspberry Pi, connect the SWD pins to the SPI GPIOs.
Tie MOSI to MISO through a series resistor (typically 1 kΩ) and use that
node as `SWDIO`. This driver does **not** assume the underlying spidev
supports 3-wire mode. The [Pi pinout for SPI0](https://pinout.xyz/) is:

| GPIO Pin | Function  | Pin-header number |
|----------|-----------|-------------------|
| GPIO 10  | SPI0 MOSI | 19                |
| GPIO 9   | SPI0 MISO | 21                |
| GPIO 11  | SPI0 SCLK | 23                |

Run `raspi-config` to enable SPI on the Raspberry Pi, then reboot. After
rebooting, you should see the spidev device at `/dev/spidev0.0`. You can
then use this device with probe-rs to connect to your target via SWD.

### Probe selection

Select the device directly with a synthetic selector that carries the
spidev path in the serial portion:

```bash
probe-rs info --probe 0:0:/dev/spidev0.0
```

The VID:PID portion is ignored. For safety, `probe-rs list` only exposes
explicit `/dev/spidev_swd*` udev links, so probe-rs does not implicitly try
every SPI device on the system.

### Cross-compiling for the Raspberry Pi

```bash
# pi1, pi-zero
cross build -p probe-rs-tools --target arm-unknown-linux-gnueabihf --release --features remote
# pi3, pi4, pi5
cross build -p probe-rs-tools --target aarch64-unknown-linux-gnu   --release --features remote

# copy the resulting binary to the Pi
scp target/arm-unknown-linux-gnueabihf/release/probe-rs pi-zero-w:~/
```

### Running on the Raspberry Pi

Direct use on the Pi:

```bash
./probe-rs info --protocol swd --probe "0:0:/dev/spidev0.0" --speed 1000
```

To run as a remote server, create a server config TOML file on the Pi as
[described in the docs](https://probe.rs/docs/tools/probe-rs/) and run:

```bash
./probe-rs serve
```

With the Pi running as a server, drive it from your PC:

```bash
# check connection:
probe-rs --host ws://pi-zero-w.local:3000 --token "token" info \
    --protocol swd --speed 1000 --probe "0:0:/dev/spidev0.0"

# run a binary:
probe-rs --host ws://pi-zero-w.local:3000 --token "token" run hello_world.elf \
    --protocol swd --chip STM32F439ZITx --speed 1000 --log-file ./temp_probers_log
```

This has been tested on a Raspberry Pi Zero W with an STM32F439ZI target up
to 16 MHz, though above 4–6 MHz little additional performance was observed.
