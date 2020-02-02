# Increasing the flashing speed

First, an overview over the current speed:

OpenOCD  - 3.5 s
probe-rs - 8.1 s


## Interface speed

In a first step, we want to look at the actual protocol speed used by both tools.
OpenOCD helpfully outputs that it is using the SWD protocol, with a speed of 4000 kHz.

probe-rs doesn't output this yet, so we have to look at the source code. After a quick look,
it actually seems that the speed is unchanged, so we can assume that the default interface speed is used.
Checking the source code of the great pyocd tool, we can expect that this is a frequency of 1800 kHz currently in use.


- Speed   100 kHz -> Flash time: 40 s
- Speed   400 kHz -> Flash time: 20 s
- Speed 1_000 kHz -> Flash time: 20 s
- Speed 1_800 kHz -> Flash time:  8 s
- Speed 4_600 kHz -> Flash time:  8 s





