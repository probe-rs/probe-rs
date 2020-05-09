# target-gen

target-gen is a helper tool for probe-rs, which can be used to extract flash algorithms and target descriptions for
chips from ARM CMSIS-Packs. This will then allow you to flash the chip using probe-rs.

## Usage with CMSIS-Pack

As a first step, you need to get an appropriate CMSIS-Pack for the chip you want to flash. A good source for CMSIS-Packs
is the following ARM website: https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search

From a CMSIS-Pack, you can extract the target descriptions for probe-rs:

    cargo run -- pack <CMSIS-PACK> out/

This wil generate YAML files containing the target descriptions, which can be used with probe-rs.

## Usage with ELF files

The target-gen tool can also be used to create a target description based on an ELF file. This
requires that the ELF file adhers to the ARM CMSIS standard for flash algorithms, which
can be found at: https://arm-software.github.io/CMSIS_5/Pack/html/algorithmFunc.html


Running

    cargo run -- extract <ELF FILE> target.yml

will create a target description containing the extracted flash algorithm. The values
for the chip description itself have to be adjusted manually in the generated Yaml file.
