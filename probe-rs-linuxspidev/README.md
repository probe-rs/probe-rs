# Linux spidev SWD support for probe-rs

This crate provides a probe-rs driver for Linux spidev SWD.

## Usage on Raspberry Pi
To use this crate on a Raspberry Pi, you need to connect the SWD pins to the appropriate GPIO pins. Connect a resistor (typically 1k) between the MOSI and MISO and use the MISO as the SWDIO pin. This module does not assume the target linux-spidev supports 3-wire mode. The [PI pinout for SPI0](https://pinout.xyz/) is as follows:
| GPIO Pin | Function  | pin-header number |
|----------|-----------|-------------------|
| GPIO 10  | SPI0 MOSI | 19                |
| GPIO 9   | SPI0 MISO | 21                |
| GPIO 11  | SPI0 SCLK | 23                |

Use raspi-config to enable SPI on the Raspberry Pi, then reboot. After rebooting, you should see the spidev device at `/dev/spidev0.0`. You can then use this device with probe-rs to connect to your target device using SWD.

To cross-compile probe-rs for the PI (this is for the pi-zero-w, for other models you may need to adjust the target), you can use the following command:

```bash
# for pi1, pi-zero
cross build -p probe-rs-tools --target arm-unknown-linux-gnueabihf --release --features remote
# for pi3, 4 or 5
cross build -p probe-rs-tools --target aarch64-unknown-linux-gnu   --release --features remote
# copy the resulting binary to the Raspberry Pi
scp target/arm-unknown-linux-gnueabihf/release/probe-rs pi-zero-w:~/
```

Then run the probe-rs binary on the Raspberry Pi as a remote server:
```bash
# on pi:
./probe-rs info --protocol swd --probe "0:0:/dev/spidev0.0" --speed 1000
```

To run as a remote server, create a server config toml file on the pi as [described in the docs](https://probe.rs/docs/tools/probe-rs/), and run the server:
```bash
# run as remote server:
./probe-rs serve
```

With the pi running as server, you can run a binary from your PC:
```bash
# check connection to the pi:
probe-rs --host ws://pi-zero-w.local:3000 --token "token" info --protocol swd --speed 1000 --probe "0:0:/dev/spidev0.0"

# or run a binary:
probe-rs --host ws://pi-zero-w.local:3000 --token "token" run hello_world.elf --protocol swd --chip STM32F439ZITx --speed 1000 --log-file ./temp_probers_log
```

This has been tested on a Raspberry Pi Zero W, with an STM32F439ZI target up to 16MHz, though over 4-6MHz little performance improvement was observed.