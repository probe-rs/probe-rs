Added a probe that uses Linux spidev to emulate SWD with full-duplex SPI. Open any
existing `/dev/spidev*` node with a selector like `--probe 0:0:/dev/spidev0.0`,
while `probe-rs list` only exposes explicit `/dev/spidev_swd*` udev links.
