# target-gen

target-gen is a helper tool for probe-rs, which can be used to extract flash algorithms and target descriptions for
chips from ARM CMSIS-Packs. This will then allow you to flash the chip using probe-rs.

## Usage

As a first step, you need to get an appropriate CMSIS-Pack for the chip you want to flash. A good source for CMSIS-Packs
is the following ARM website: https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search

From a CMSIS-Pack, you can extract the target descriptions for probe-rs:

    cargo run --  <CMSIS-PACK> out/

This wil generate YAML files containing the target descriptions, which can be used with probe-rs.

