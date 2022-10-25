# target-gen

target-gen is a helper tool for probe-rs, which can be used to extract flash algorithms and target descriptions for
chips from ARM CMSIS-Packs. This will then allow you to flash the chip using probe-rs.

## Usage with CMSIS-Pack

As a first step, you need to get an appropriate CMSIS-Pack for the chip you want to flash. By default, probe-rs will look for CMSIS-Packs
at the following ARM website: https://developer.arm.com/tools-and-software/embedded/cmsis/cmsis-search.

### To download and generate pack files, use the `arm` subcommand:

`cargo run --release -- arm [OPTIONS] <OUTPUT>`

Arguments:
<OUTPUT> An output directory where all the generated .yaml files are put in.

    Options:
    -l, --list                  Optionally, list the names of all pack files available in <https://www.keil.com/pack/Keil.pidx>
    -f, --filter <PACK_FILTER>  Optionally, filter the pack files that start with the specified name,
                                e.g. `STM32H7xx` or `LPC55S69_DFP`.
                                See `target-gen arm --list` for a list of available Pack files

### If you already have a pack file, you use the `pack` submcommand:

`cargo run --release -- pack [OPTIONS] <OUTPUT>`

    Arguments:
    <INPUT>   A Pack file or the unziped Pack directory.
    <OUTPUT>  An output directory where all the generated .yaml files are put in.

This wil generate YAML files containing the target descriptions, which can be used with probe-rs.

## Usage with ELF files

The target-gen tool can also be used to create a target description based on an ELF file. This
requires that the ELF file adhers to the ARM CMSIS standard for flash algorithms, which
can be found at: https://arm-software.github.io/CMSIS_5/Pack/html/algorithmFunc.html

Running

    cargo run --release -- elf <ELF FILE> target.yml

will create a target description containing the extracted flash algorithm. The values
for the chip description itself have to be adjusted manually in the generated Yaml file.
