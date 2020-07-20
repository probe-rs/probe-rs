[33mcommit a0e5d330650d6ef5ec3f94e26134b06e281242ba[m[33m ([m[1;36mHEAD -> [m[1;32mfix-dap[m[33m)[m
Merge: 4537bf8 56dd8ed
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jul 14 18:26:28 2020 +0200

    Merge branch 'fix-dap' of github.com:probe-rs/probe-rs into fix-dap

[33mcommit 4537bf8cc2673ddadf17e681cfc38d1fbbac5105[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jul 14 18:25:55 2020 +0200

    Add CHANGELOG entry

[33mcommit ff98d9070eadd826860fc81f6f7a88257f4de512[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jul 14 18:08:54 2020 +0200

    Open a DAPv2 even if only the VID/PID pair matches and no serial was given

[33mcommit 56dd8edf094c6aed322eb23a1c91b16beb2da047[m[33m ([m[1;31morigin/fix-dap[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jul 14 18:08:54 2020 +0200

    Open a DAPv2 even if only the VID/PID pair matches and no serial was given

[33mcommit 58d67615bb16a1c34d7a02ba5ea55cb1cd4f0ab8[m[33m ([m[1;31morigin/master[m[33m, [m[1;31morigin/HEAD[m[33m, [m[1;32mmaster[m[33m)[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue Jul 14 07:31:31 2020 +0000

    Update colored requirement from 1.8.0 to 2.0.0 in /gdb-server
    
    Updates the requirements on [colored](https://github.com/mackwic/colored) to permit the latest version.
    - [Release notes](https://github.com/mackwic/colored/releases)
    - [Changelog](https://github.com/mackwic/colored/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/mackwic/colored/compare/v1.8.0...v2.0.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 29e2606373db6c43e94bc12d79d88dfb3d86685c[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Sun Jul 12 15:05:37 2020 +0200

    cargo fmt

[33mcommit ea6ff6af21fb1ee7bd2cb6422c9aff1a549c1ea4[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Sun Jul 12 14:56:37 2020 +0200

    Make more return types public
    
    * CoreInformation is used as return type in our debugging functions (halt, step etc.)
    * Architecture is the return type of the architecture functions, defined
      for Core, Session etc.

[33mcommit ab592caa2567f6ae04de2bd03f84b57c3715d41b[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Jul 3 16:27:24 2020 +0200

    Use anyhow in the probe-rs-cli, so that wrapped errors are shown

[33mcommit aac2462c45c9647d249eeeedff5f3a4931b7b879[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Jul 7 12:54:36 2020 +0300

    Add a comment about unsupported firmware version

[33mcommit 4d3fa4ae09681de2896c42191c8ee287b3604a74[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Jul 7 12:39:03 2020 +0300

    Add firmware version check for STLINK-V3 probes

[33mcommit d6e1717634c90cd6b12c3bedd2306c22ccda1741[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Jul 6 07:50:14 2020 +0000

    Update gimli requirement from 0.21.0 to 0.22.0 in /probe-rs
    
    Updates the requirements on [gimli](https://github.com/gimli-rs/gimli) to permit the latest version.
    - [Release notes](https://github.com/gimli-rs/gimli/releases)
    - [Changelog](https://github.com/gimli-rs/gimli/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/gimli-rs/gimli/compare/0.21.0...0.22.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 906693506fa1e590cfda445d65db2a2882fa7060[m[33m ([m[1;33mtag: v0.8.0[m[33m)[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Jun 30 00:03:22 2020 +0200

    Update CHANGELOG.md
    
    Co-authored-by: Danilo Bargen <mail@dbrgn.ch>

[33mcommit a2eb63b50a5d4be9edb001b81edabe962b57f21f[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Jun 30 00:03:14 2020 +0200

    Update CHANGELOG.md
    
    Co-authored-by: Danilo Bargen <mail@dbrgn.ch>

[33mcommit 3be8677436cfde985831893878977fab20c32ca7[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Jun 30 00:02:48 2020 +0200

    Update CHANGELOG.md
    
    Co-authored-by: Danilo Bargen <mail@dbrgn.ch>

[33mcommit 3a09a2cd6afc544560cdf9504f38b96df8e4027d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 29 23:35:45 2020 +0200

    Fix missed things in changelog

[33mcommit c1f7ca673de34cd46567b8fc169d02e84e688f1b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 29 23:30:02 2020 +0200

    Prepare for 0.8.0 & Update CHANGELOG

[33mcommit 6bb3c13eb43746078e62a05ec1b947412e2b6956[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jun 23 17:57:16 2020 +0200

    Fixed a bug where a probe-selector would not work for the JLink if only VID & PID were specified but no serial number

[33mcommit 4568f965057e9505c613d87a1c771d33b735ded5[m
Author: Danilo Bargen <mail@dbrgn.ch>
Date:   Sat Jun 27 01:32:31 2020 +0200

    J-Link: Log some useful information when attaching
    
    - Serial number
    - Firmware version
    - Hardware version
    - Capabilities

[33mcommit 3b0309b028207deca3892389c69d656b188eec16[m
Author: Danilo Bargen <mail@dbrgn.ch>
Date:   Sat Jun 27 00:38:46 2020 +0200

    J-Link: Check target voltage before attaching
    
    If the target voltage is 0 V, a warning is generated. This can happen
    if the target is not powered, or if the VTref pin is not properly
    connected.

[33mcommit 87dddcbcd07b387fb0e6bdb7c052609aba94713a[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Fri Jun 26 13:10:26 2020 +0200

    Update probe-rs/src/probe/stlink/tools.rs

[33mcommit 77e8812f86def6dd686a290817d4227519431493[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jun 20 15:58:24 2020 +0200

    Show ST-Link in probe list even if accessing the serial number fails

[33mcommit 940174cc1c2a7d5368f536b778c337f511620c99[m
Author: Hanno Braun <hanno@braun-embedded.com>
Date:   Thu Jun 25 14:39:24 2020 +0200

    Derive `Debug` for `Session`
    
    Also derives `Debug` for a bunch of other types, as required.

[33mcommit c1d65df06168e642132b890523308e32bc542ae7[m
Merge: 0c6787e dda38ea
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Tue Jun 23 19:59:16 2020 +0200

    Merge pull request #271 from probe-rs/configurable-timeout
    
    Configurable timeout

[33mcommit 0c6787e687899c4be7fefefd22311eb4cf8396e9[m
Author: Ralf Fuest <mail@rfuest.de>
Date:   Sun Jun 21 21:31:24 2020 +0200

    Add changelog entry

[33mcommit ab8ef4ec89f9e312d8fdc5ab3e0d227f3bbced98[m
Author: Ralf Fuest <mail@rfuest.de>
Date:   Sun Jun 21 20:19:00 2020 +0200

    Add STM32F7 target descriptions

[33mcommit 7c78c48b7701df47a1cfbf2882d6d0fd8ecf0eec[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jun 20 17:29:30 2020 +0200

    Adapt code to new ihex API

[33mcommit 142e23c621bb53b57afaea5d4cbcc8a12526e64e[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Sat Jun 20 10:37:56 2020 +0000

    Update ihex requirement from 1.1.2 to 3.0.0 in /probe-rs
    
    Updates the requirements on [ihex](https://github.com/martinmroz/ihex) to permit the latest version.
    - [Release notes](https://github.com/martinmroz/ihex/releases)
    - [Commits](https://github.com/martinmroz/ihex/compare/v1.1.2...v3.0.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit dda38ea0ae69952fe7ddec0721f73a47007e25ad[m[33m ([m[1;31morigin/configurable-timeout[m[33m, [m[1;32mconfigurable-timeout[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Jun 20 12:47:08 2020 +0200

    Fix doctests

[33mcommit ba2bd2d7c60859669d3872110c643da53ac46b3e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jun 5 17:02:46 2020 +0200

    make the flasher timeouts more configurable

[33mcommit bbaee5aaf6ada8f42059d653ad5d9e19b8490a8f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jun 5 16:47:20 2020 +0200

    Make the timeout configurable for the core halted check function

[33mcommit b5a1ae727bf661bf8eb53e6964052b0ba0458eb4[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Jun 15 07:57:55 2020 +0000

    Update object requirement from 0.19.0 to 0.20.0 in /probe-rs
    
    Updates the requirements on [object](https://github.com/gimli-rs/object) to permit the latest version.
    - [Release notes](https://github.com/gimli-rs/object/releases)
    - [Commits](https://github.com/gimli-rs/object/compare/0.19.0...0.20.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 505cc99070cae364ed8fa7cb5ef89fe0c989ba57[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Jun 15 07:45:42 2020 +0000

    Update ihex requirement from 1.1.2 to 3.0.0 in /cli
    
    Updates the requirements on [ihex](https://github.com/martinmroz/ihex) to permit the latest version.
    - [Release notes](https://github.com/martinmroz/ihex/releases)
    - [Commits](https://github.com/martinmroz/ihex/compare/v1.1.2...v3.0.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 47969628d7c55d67e1d6c76976fa0a7997743f65[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue Jun 16 07:25:33 2020 +0000

    Update rusb requirement from 0.5.5 to 0.6.0 in /probe-rs
    
    Updates the requirements on [rusb](https://github.com/a1ien/rusb) to permit the latest version.
    - [Release notes](https://github.com/a1ien/rusb/releases)
    - [Commits](https://github.com/a1ien/rusb/commits/0.6.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 80045c155f0dfb174a8ce6860d36be412f8d0eab[m
Author: Erik Svensson <erik.public@gmail.com>
Date:   Tue Jun 16 18:19:44 2020 +0200

    Added a note in the changelog

[33mcommit 9bff273658d24b758d433050a332fbb984e5c4ef[m
Author: Erik Svensson <erik.public@gmail.com>
Date:   Tue Jun 16 09:06:08 2020 +0200

    Updated target file for nRF52 family
    
    Generated from,
    NordicSemiconductor.nRF_DeviceFamilyPack.8.32.1.pack

[33mcommit 3cb65aec9b543c27951f0c08237fba46731635a5[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Jun 14 10:02:55 2020 +0300

    Add generic Cortex-M7 target

[33mcommit 2cd613fb440fdd20c1246ea5c49aee561aeb51cb[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat Jun 6 23:10:33 2020 +0200

    Slightly clean up error handling to get a better chain of errors
    
    E.g.
    ```
    Caused by:
        0: Error while flashing
        1: Something during the interaction with the core went wrong
        2: While trying to halt
        3: Waiting for halted core
        4: An error with the usage of the probe occured
        5: Operation timed out
    ```
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit d2a062d6ac9908add830e01d898ada74fdb465ce[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat Jun 6 16:48:53 2020 +0200

    Allow transparent passthrough of anyhow errors
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit b8360458d0d1d7d8efac8779b6513f55628f6c61[m
Merge: da31e07 e50c21a
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Jun 10 16:00:01 2020 +0100

    Merge pull request #269 from probe-rs/add-timeout-for-cmsis-dap
    
    Add timeout for CMSIS-DAPv1 read operations

[33mcommit e50c21a6ed5ac3685b44f94a1212a8038f62fd4c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jun 10 14:59:10 2020 +0200

    Add timeout for CMSIS-DAPv1 read operations

[33mcommit da31e07b922f7c0753d5bad5de50f08d4f838f5e[m
Author: Mathias Brossard <mathias@brossard.org>
Date:   Tue Jun 2 23:28:46 2020 -0500

    Run `cargo fmt`

[33mcommit 76ea45fce8f5815ce5f2d0dd2f3dc89891e87580[m
Author: Mathias Brossard <mathias@brossard.org>
Date:   Tue Jun 2 21:15:08 2020 -0500

    Annotate structs to not serialize Option fields when value is None

[33mcommit 8ab3940683dacc8a59e23f67a9b57e69d8d70115[m[33m ([m[1;33mtag: v0.7.1[m[33m, [m[1;33mtag: v0.7.0[m[33m)[m
Author: Thales Fragoso <thales.fragosoz@gmail.com>
Date:   Wed Jun 3 16:34:30 2020 -0300

    Prepare 0.7.1 release

[33mcommit bf450d591b13bb2111b09b36747be0e177da4f7d[m
Author: Thales Fragoso <thales.fragosoz@gmail.com>
Date:   Tue Jun 2 20:00:32 2020 -0300

    Make DebugProbeType public

[33mcommit 4b8ad8842941a07bacf07dbf3ef9b1594bf0df75[m
Author: Mathias Brossard <mathias@brossard.org>
Date:   Tue Jun 2 21:19:53 2020 -0500

    Update LPC55S69 and LPC55S66 targets

[33mcommit c9449bfc6772f23324f71059a4e988c1f5195b1a[m
Author: Mathias Brossard <mathias@brossard.org>
Date:   Tue Jun 2 23:52:10 2020 -0500

    Add missing core value for LPC55S66 and LPC55S69

[33mcommit 056c8d1e1f7fc9f35e248156904f9d31cdc71f56[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Jun 3 17:41:59 2020 +0200

    Prepare 0.7.0 release (#261)
    
    * Prepare 0.7.0 release
    
    Co-authored-by: Thales <46510852+thalesfragoso@users.noreply.github.com>

[33mcommit 2127cd3a11930b461265d46c137aabe58e46fee1[m
Merge: c0eb70e 60d7a75
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Jun 1 21:08:25 2020 +0200

    Merge pull request #260 from probe-rs/remove-manual-error-stacking
    
    Start using anyhow for errors and get rid of manual error stacking

[33mcommit 60d7a7533e49a888eee361f8dcc81487f35bbedc[m[33m ([m[1;31morigin/remove-manual-error-stacking[m[33m)[m
Merge: 1349598 c0eb70e
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Jun 1 20:59:38 2020 +0200

    Merge branch 'master' into remove-manual-error-stacking

[33mcommit c0eb70e9c9e8a8ca3b04936f7aa5c26ea03649a8[m
Merge: e71105f 970d4de
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Jun 1 20:48:48 2020 +0200

    Merge pull request #245 from probe-rs/improve-backtraces
    
    Improve backtrace handling

[33mcommit 970d4de4595096d33208aefac61fb45f9de67256[m
Merge: 89c535b e71105f
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Jun 1 20:44:22 2020 +0200

    Merge branch 'master' into improve-backtraces

[33mcommit e71105f3498e4106ffbdedd00ae0da3d2820f280[m
Merge: 1d9f5da 6288133
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Jun 1 20:42:56 2020 +0200

    Merge pull request #246 from probe-rs/serial-id
    
    Honor serial numbers of probes

[33mcommit 1349598a5dc1d32d591b048c1a7515ad768150d5[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Mon Jun 1 19:30:47 2020 +0200

    Start using anyhow for errors and get rid of manual error stacking
    
    In conjunction with `anyhow` enabled applications this generates nicer
    error messages like:
    ```
    Error failed to flash /Users/egger/OSS/stm32f429i-disc/target/thumbv7em-none-eabihf/release/examples/serial_echo
    
    Caused by:
        0: Error while flashing
        1: Something during the interaction with the core went wrong
        2: An error with the usage of the probe occured
        3: Operation timed out
    ```
    
    or
    
    ```
    Error failed attaching to target
    
    Caused by:
        0: An error with the usage of the probe occured
        1: An error specific to a probe type occured
        2: Command failed with status JtagGetIdcodeError
    ```
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 1d9f5da33a2a6ca409a2618343c1eb5bdf222e8b[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Jun 1 08:03:51 2020 +0000

    Update enum-primitive-derive requirement in /probe-rs
    
    Updates the requirements on [enum-primitive-derive](https://gitlab.com/cardoe/enum-primitive-derive) to permit the latest version.
    - [Release notes](https://gitlab.com/cardoe/enum-primitive-derive/tags)
    - [Changelog](https://gitlab.com/cardoe/enum-primitive-derive/blob/master/CHANGELOG.md)
    - [Commits](https://gitlab.com/cardoe/enum-primitive-derive/compare/0.1.2...v0.2.1)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit f867b5c743ec46682fedc993c6ab3d79481e8a23[m
Author: Thales <46510852+thalesfragoso@users.noreply.github.com>
Date:   Sun May 31 20:28:22 2020 -0300

    Add docs suggested in code review
    
    Co-authored-by: Yatekii <Yatekii@users.noreply.github.com>

[33mcommit 5a8fc4ad84c7a82fa6ad56661819fec58185e97d[m
Author: Thales Fragoso <thales.fragosoz@gmail.com>
Date:   Wed May 27 23:28:00 2020 -0300

    Remove custom-ap
    
    Also add the possibility to implement it in an external crate

[33mcommit 936f633cd07c72119637e2c78f5e2b25b79751ec[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat May 30 20:07:45 2020 +0200

    Reduce amount of unwraps
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 03f34798e6f28bca00191226b1a87456ff63b778[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat May 30 22:13:25 2020 +0200

    Only build regular pushes to master
    
    Prevents building each PR multiple times

[33mcommit f28ed0007e67f90f306d3cce01715c398c7c5b92[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed May 27 23:52:49 2020 +0100

    Fix returning error on empty daplink batch

[33mcommit 6288133613bd2878305455b8f15adc068e291b20[m
Merge: 751f0b5 a161898
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun May 24 23:35:50 2020 +0200

    Merge branch 'master' into serial-id

[33mcommit 751f0b5056c6deb93f7fcf08ede221e343713b82[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun May 24 03:01:59 2020 +0100

    Run rustfmt

[33mcommit b2040f14c86e710365e402ce7bb27d7a8defcb82[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun May 24 02:58:15 2020 +0100

    Rework DAPlink scanning for new ProbeSelector

[33mcommit a161898634c7c63f88c5b4667e7c0a22022d2f99[m
Author: Thales Fragoso <thales.fragosoz@gmail.com>
Date:   Fri May 22 00:46:32 2020 -0300

    modifications to allow an external nrf-recover utility

[33mcommit 9c318bf12465cacf684ae2990fc031c1b8ab3d77[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Fri May 22 07:37:40 2020 +0000

    Update ron requirement from 0.5.1 to 0.6.0 in /cli
    
    Updates the requirements on [ron](https://github.com/ron-rs/ron) to permit the latest version.
    - [Release notes](https://github.com/ron-rs/ron/releases)
    - [Changelog](https://github.com/ron-rs/ron/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/ron-rs/ron/compare/v0.5.1...v0.6.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit df3c00b80ad3f16f80b4ad760b4b559f21f2f3df[m
Author: Aurélien Jacobs <aj@technolia.fr>
Date:   Wed May 20 19:48:23 2020 +0200

    Fix stlink version check
    
    Current code check if version < 28 and if not, it uses open_ap().
    But then open_ap() ensure that version > 28.
    So currently stlink with verion 28 is not working at all.
    open_ap() should instead check that version >= 28.

[33mcommit 3d48276e83cede5c69366f4724ac03bc4274d157[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue May 12 12:46:39 2020 +0000

    Update object requirement from 0.18.0 to 0.19.0 in /probe-rs
    
    Updates the requirements on [object](https://github.com/gimli-rs/object) to permit the latest version.
    - [Release notes](https://github.com/gimli-rs/object/releases)
    - [Commits](https://github.com/gimli-rs/object/compare/0.18.0...0.19.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 9f2f4577b875756b0e24a2d8fe696bd159ba878e[m[33m ([m[1;31morigin/serial-id[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 16:32:33 2020 +0200

    Fix a bug where an stlink could open a jlink accidentially

[33mcommit ba415d5a4daee12229385194de0503891812f7a2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 16:18:13 2020 +0200

    WIP

[33mcommit 8c9ced81a719db6bb53be466fe3d28dc096c2110[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 03:51:29 2020 +0200

    Make memory regions visible again

[33mcommit 186bbe97f88aaa4083f2549f3ee1665224f1b696[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 03:14:30 2020 +0200

    Improve errorhandling on probe creation

[33mcommit ed0c89bd5e3c5a57a7d5425e995f157c6bfaac4c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 01:50:43 2020 +0200

    Change probe selector parsing to hex radix

[33mcommit 51caa61d69e916d10628a6250ec75f0081174a61[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 01:28:08 2020 +0200

    Add docs

[33mcommit c6fa86dd587270154cb78b45c4f433c93c1cb1cf[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 15 01:17:26 2020 +0200

    Add a DebugProbeSelector which can be used for selecting the probe

[33mcommit 1343a42b2096de7bae432dd124d19b6b86502c09[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 13 23:50:32 2020 +0200

    Honor the serial number when opening an STLink

[33mcommit 6d4e995e84a3c36484ba6f9b6d80cd12ed172dbe[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 13 23:21:39 2020 +0200

    Respect the serial number when attaching to a jlink

[33mcommit 8f9f89b8abc175d73bc29492915e50ebc73b365a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 13 22:54:57 2020 +0200

    JLink scan now includes the serial number

[33mcommit 271a0e82601ab23550ddc624c54b4a5dc9dee0fa[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 13 22:32:49 2020 +0200

    Add serial readout to jlink device info; does not work somehow

[33mcommit 09e66de23c57e98bb0b6212b12e57ad51d7f524c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 13 19:55:14 2020 +0200

    Add serial number readout to stlink handling

[33mcommit c4b9ea73f6f1aff113f712ec1946b70e9bcadeb3[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue May 12 07:51:45 2020 +0000

    Update gimli requirement from 0.20.0 to 0.21.0 in /probe-rs
    
    Updates the requirements on [gimli](https://github.com/gimli-rs/gimli) to permit the latest version.
    - [Release notes](https://github.com/gimli-rs/gimli/releases)
    - [Changelog](https://github.com/gimli-rs/gimli/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/gimli-rs/gimli/compare/0.20.0...0.21.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 89c535b9800eadfe54f2778a22dc77aa8612f05e[m[33m ([m[1;31morigin/improve-backtraces[m[33m)[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun May 10 12:38:17 2020 +0200

    Improve backtrace handling

[33mcommit 6857c3f553d21d66b8d897a2b783701b8fa5cd51[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri May 8 12:05:13 2020 +0200

    Fix formatting

[33mcommit 779a0a4dfc3e33e4fbb878357044ee9f96fdd5ef[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue May 5 21:07:53 2020 +0200

    Cleanup GDB server

[33mcommit 9cd365f0f256eaead1f024a066ceafa9bfe238b6[m
Merge: 12615f9 7b46c8a
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu May 7 20:28:40 2020 +0200

    Merge pull request #240 from probe-rs/api-changes
    
    API changes to make multithreading easier.

[33mcommit 7b46c8ac2de66183fce30616879a18b4f4770908[m[33m ([m[1;31morigin/api-changes[m[33m, [m[1;32mapi-changes[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 23:38:34 2020 +0200

    Clean some more lifetimes

[33mcommit 0a6a8b2b69a6b037d964278ee8c3c00177ef79b1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 23:35:22 2020 +0200

    Fix clippy lints

[33mcommit 717896da0f0dc207a6a339f588b36b9365b7c871[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 23:30:16 2020 +0200

    Fix doc comment with wrong info

[33mcommit a485eb40daaed9c96deba9c6a3d705ac852479e6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 23:29:10 2020 +0200

    Make the M cores only poll the state on first construction

[33mcommit 1da8e455156056c7c57681488ad0aada7b14cf37[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 23:27:13 2020 +0200

    Revert auto_attach rename

[33mcommit dcce4135f8e111efb90dcef488296bb21f6082fe[m
Merge: 0729532 12615f9
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 00:53:10 2020 +0200

    Merge branch 'master' into api-changes

[33mcommit 07295326f37d7dda744541fa633c7e825c49295b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 00:38:46 2020 +0200

    Fix doctests and also rename the two attach functions

[33mcommit 82c832bdb3118b9aa7bc444b0fbd91794a5df876[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 6 00:26:28 2020 +0200

    Make the inner core state persistent

[33mcommit caf59cf955cb0f7156ea447b9fa7225fdf502b69[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue May 5 23:45:33 2020 +0200

    Address some feedback

[33mcommit 0c6bd36b9da4961afeb6ea359543fa271326b104[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue May 5 21:39:09 2020 +0200

    Clean up lifetimes with properly naming them for better tracking and removing unneeded ones

[33mcommit 12615f9ad739591b09e27448758e310a8ec65aff[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon May 4 15:28:20 2020 +0200

    Adress review feedback

[33mcommit db457bbe9e86c734970ce92859a81b2a5c9e1fd0[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Apr 19 21:29:56 2020 +0200

    Improve memory read/write speed for RISCV

[33mcommit b3a1e299ea9a313eae2b5a9dd186f9c648d07f41[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Apr 18 21:43:18 2020 +0200

    Support for RISCV Flashloader

[33mcommit b351bc1132d0ff7fee26d32af6342ed9683cf040[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon May 4 16:28:43 2020 +0200

    Fix a doctest; second one to be fixed

[33mcommit 86b21c87725d41b457708702d7911d69649be0a3[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon May 4 16:26:41 2020 +0200

    Hopefully fix recursion

[33mcommit 6993d6b6d4ff7a35cea97634bb0a624d3006d2e0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon May 4 15:44:16 2020 +0200

    Fixed warnings

[33mcommit f50365d4b07c7b2c3ffe6b94e6c04653ebc63971[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon May 4 14:47:56 2020 +0200

    Applications are adjusted and Riscv fully enabled; Core state is now stored properly

[33mcommit 0031986157544621f9b4ff98341fa5952241e40f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun May 3 11:12:48 2020 +0200

    Fix broken vCont and memory-map, add LLDB support
    
    LLDB is using some different commands, which we don't support (yet).
    It gets confused when we reply with 'OK' to these, so we just send an empty
    response now. With these changes, LLDB seems to work pretty well.
    
    See also: https://github.com/llvm/llvm-project/blob/master/lldb/docs/lldb-gdb-remote.txt

[33mcommit 61fc04d51707aec185958df9d01d9b11224209eb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 2 17:37:28 2020 +0200

    WIP: Base impl done

[33mcommit 43858fb8e76a7b040cdbfc2a25936114023a1591[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Thu Apr 30 12:49:17 2020 +0200

    CHANGELOG for the gdb-server fixes (#237)
    
    * Update CHANGELOG.md with the gdb-server fixes
    
    Co-Authored-By: Yatekii <Yatekii@users.noreply.github.com>

[33mcommit 46466f6971666a29fb90df082623d2d7aafd60d5[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Wed Apr 29 22:03:22 2020 +0200

    Performance and functionality improvements for gdb (#236)
    
    * Performance and functionality improvements for gdb
    
    * Make the endlessly polling function await_halt in worker.rs
    sleep for 1ms on every iteration so we don't constantly draw
    100 % from the CPU core we're running on
    
    * Return a T02 instead of a T05hwbreak when a user requests a
    break via an interrupt with Ctrl-c (stolen from OpenOCD, I don't
    actually know the meaning of the 02)
    
    * Previously await_halt would send a constant stream of T05hwbreak
    to the GDB client once the core was haltet as await_halt was never
    set to false when a breakpoint was hit, change this behaviour so
    breaking actually works.

[33mcommit 626d9114bdb6c8aa7645109a5afb7e1254130441[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 27 14:16:45 2020 +0200

    Add a test to test successfull serialization to prevent further breakage

[33mcommit a259c685c3551c5f5c1f4834903c3bda0c2bcdf2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 27 14:02:44 2020 +0200

    Fix serialization

[33mcommit 57a658fc9fb1a0de554a66057acaac7a1b4f8513[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Apr 23 20:03:19 2020 +0200

    Show correct error message when no probe is detected

[33mcommit 1c08920ed5cdd2bad62619cf8c08c5865008ec7c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Apr 18 22:54:50 2020 +0200

    Implement status function for all ARM cores

[33mcommit b0b68ede3900463883c219e1a74b4e79dc490e93[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Apr 16 17:03:57 2020 +0200

    Fix clippy warnings

[33mcommit df6a8afb18a971bb04c1be2c34ea4fd3c4edbc49[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Apr 15 22:03:50 2020 +0200

    Rework status handling for Cortex M4

[33mcommit 5195ec2a0624f83245caa91d206f7949d82cc978[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Apr 15 20:34:01 2020 +0200

    Add state function for RISCV

[33mcommit 4cb300844a5b7baa60e2dd1b32b460653e0ead3f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Apr 15 14:28:29 2020 +0200

    Initial version of status function, for M4 only

[33mcommit fc4f45f870f4c494ff025109fe89fd495a27268d[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Apr 20 19:57:47 2020 +0200

    Drop USB handle after reading serial number

[33mcommit 947487a2c6c98d5e57ff5bca5e15c2c575c6f433[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 19:38:27 2020 +0100

    Run rustfmt

[33mcommit 9419f53f595d1b058d6fe4339c907ed267fe3721[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 19:21:53 2020 +0100

    Improve CMSIS-DAP v1 fallback further; set packet lengths more appropriately

[33mcommit 061dcc230b948c9a1a79149550564e9d989691aa[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 17:51:43 2020 +0100

    Tidy up fallback HID use

[33mcommit 0dc02f2bd93975e2fbaaa256ac0b28be91a65b09[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 16:12:47 2020 +0100

    Fallback to using hidapi to enumerate HID devices if rusb fails

[33mcommit 61667d635d3b64d16ca98f53f74892511b5eeba7[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 04:03:54 2020 +0100

    Implement CMSIS-DAP v2 protocol alongside v1

[33mcommit c3e58dc950c2116cd223fdf92ba27e2fa34c1370[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 01:09:08 2020 +0100

    Add new USBError variant on CmsisDapError

[33mcommit ae2a88bce1104ddcf081d0612408d09bf2c51661[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Sun Apr 19 01:08:20 2020 +0100

    Display probe info with hex VID/PID

[33mcommit dc0ab0962d1df8663599b26ff48e9886c600ee95[m[33m ([m[1;33mtag: v0.6.2[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 20 00:13:18 2020 +0200

    Make everything ready for 0.6.2

[33mcommit 30b5a9f24f17e181ea42b4b45f54d21296555d97[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 20 00:09:08 2020 +0200

    Add Serialize to WireProtocol

[33mcommit 168ee2edb7068d83c45c6108eb8220f40a9f721c[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Apr 19 20:37:46 2020 +0200

    Return error code if an invalid memory location is being accessed (#229)
    
    * Return error code if an invalid memory location is being accessed

[33mcommit 04796be3283e453ba6d0cfe24fc3f087d3747608[m[33m ([m[1;33mtag: v0.6.1[m[33m)[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Apr 15 21:53:45 2020 +0200

    Make probe-rs ready for 0.6.1 (#227)
    
    * Bump versions
    
    * Update changelog

[33mcommit 3e66e4ad0538876b5f17e4356c6d8705b1f1938b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 13 23:49:11 2020 +0200

    Clean up debug impl for the various flash structs

[33mcommit 5416553b51f276738f459b97e53808073ab91ce6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Apr 13 23:43:03 2020 +0200

    Remove insta from tests

[33mcommit 3ff2ec75dbcae8f04f8221a55f35b92735dfd08e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Apr 12 21:03:45 2020 +0200

    Try and make tests not insta dependent

[33mcommit 13f8c1a0c4d4089745249279bffd190371fbb111[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Apr 9 21:22:33 2020 +0200

    make the WireProtocol enum deserializable

[33mcommit 7350beb36f8867e58e37bfe73b89d93e626ddfe9[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Apr 13 22:19:39 2020 +0200

    Address review feedback

[33mcommit 2a653f72e1561bf792b28b6732855d4de5e60381[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Apr 10 19:30:05 2020 +0200

    Add tests for version check

[33mcommit 7e6d7f4b22e5c193302802afa2fdee2261176943[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Apr 10 13:09:21 2020 +0200

    Improve handling of ST-Links which don't support multiple APs

[33mcommit 1249594acfa3b0b859dcb21bb33f50e04a25c8be[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Sun Apr 12 19:06:09 2020 +0200

    Adding support for FPB Unit rev 2 breakpoints.

[33mcommit dc9fb25c669727ae940bded62938d7f545e281c0[m
Merge: 50488dd ca41418
Author: Henrik Böving <hargonix@gmail.com>
Date:   Mon Apr 13 13:07:58 2020 +0200

    Merge pull request #225 from probe-rs/dependabot/cargo/cli/capstone-0.7.0
    
    Update capstone requirement from 0.6.0 to 0.7.0 in /cli

[33mcommit ca414180c98dbc9ce54cbf60dc601a6677ee410c[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Apr 13 08:03:23 2020 +0000

    Update capstone requirement from 0.6.0 to 0.7.0 in /cli
    
    Updates the requirements on [capstone](https://github.com/capstone-rust/capstone-rs) to permit the latest version.
    - [Release notes](https://github.com/capstone-rust/capstone-rs/releases)
    - [Changelog](https://github.com/capstone-rust/capstone-rs/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/capstone-rust/capstone-rs/compare/capstone-v0.6.0...v0.7.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 50488dd0890fb9dd9ab7e5131738ef7ad3b7d3c2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Apr 3 23:56:24 2020 +0200

    Adapt CLI to new attach

[33mcommit 83438c0050ba2574faaf56614d71f991e049d89f[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Apr 8 23:52:50 2020 +0100

    Add CHANGELOG entry for DAPlink queuing

[33mcommit 61675716171074d312ceadd91a031b4deb6d1bf4[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Apr 8 13:15:39 2020 +0100

    Replace dummy reads of RDBUFF with dummy writes to CSW to allow better batching by daplink

[33mcommit ed0892bce50d1b7750a439946f1a0c2dc5c79925[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Apr 8 12:36:08 2020 +0100

    rustfmt

[33mcommit 8b303ab2e8f66dbdd42eb3e115c0e223e4045d0d[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Apr 8 12:26:56 2020 +0100

    Remove unused direct_read/write_register

[33mcommit ad656e845aad1201f8547721f809d573ea36c9e8[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Wed Apr 8 12:22:59 2020 +0100

    Automatically batch DAPlink writes

[33mcommit 3ce580d2d616cf94718c988d5ee3381682b6bbd4[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 19:03:32 2020 +0100

    Add snap for build_sectors_and_pages without empty pages

[33mcommit 3980415d9dec753427b5556d74065d524755cf40[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 18:55:41 2020 +0100

    Modify insta snaps for fixed FlashFill page_index

[33mcommit ed1ccf80d06237521ca41439ef4b168295c3aaea[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 18:44:23 2020 +0100

    Skip empty pages when not restoring

[33mcommit 8e3c3f737fc6bd5841759ab9191b391b8f592bb1[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 17:32:10 2020 +0100

    Drain USB HID buffer at connection

[33mcommit ca0894599162a5d589b462557e88bcf91cc66493[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 14:47:08 2020 +0100

    Fix it's->its typos

[33mcommit 29b788dc03fafe7ffcd4410f2ebde3ad2bb0823b[m
Author: Adam Greig <adam@adamgreig.com>
Date:   Tue Apr 7 12:55:22 2020 +0100

    Support setting SWD speed on DAP probes

[33mcommit 282200dc7ac0220808cffeb35d4ce717074b4b61[m
Author: Ferdia McKeogh <ferdia@mckeogh.tech>
Date:   Fri Apr 3 12:04:00 2020 +0100

    Add STM32F3 support

[33mcommit 39917904c1c53c4e46248cf535b5fa3f744c0858[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Apr 4 23:05:12 2020 +0200

    address feedback

[33mcommit 57fdc9b32f0dd8cd4df9a2cbc6ef23f99e18684c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Apr 4 16:47:02 2020 +0200

    Make stlink status logging and handling nicer

[33mcommit 7a826f483d83499bdb28b1bd8f457a3ad75ad2b9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Apr 4 15:15:16 2020 +0200

    Make sure hard reset is not crashing any probe driver

[33mcommit f4161de0378465d4a7f816fa16361ce0aaac001c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 30 23:48:13 2020 +0200

    Fix deployment

[33mcommit a73f64f2caec97a91d110e1f6076552bf0672e0d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 30 23:47:19 2020 +0200

    Fix deployment

[33mcommit 353ad4f4c77d84b10b688f20b6f5475dd265accd[m
Author: Ein Terakawa <applause@elfmimi.jp>
Date:   Sat Apr 4 10:10:59 2020 +0900

    [FIX] typo: tms_enter_idle -> tdi_enter_idle

[33mcommit e1cfe70473ee4d6ae5495bbf4c2bd2467496367a[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Thu Apr 2 20:13:52 2020 +0200

    also adding the integer overflow check to intersects_range

[33mcommit 8d2b77e8fc64823ac408ef3b7a5c0f3beb26c016[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Wed Apr 1 23:23:49 2020 +0200

    Adding support for M7 and STM32H7
    
    Add generic support for M7 chips by aliasing them to the same behaviour
    as the M4 and the M3. Furthermore add the STM32H7 series as a new chip
    family. I added them with a reduced sets of flash algorithms as all the
    others were
    1. For special eval boards
    2. Caused false positives in the flash algorithm detection which made
       cargo flash not work correctly anymore
    
    TODO: GDB support is behaving kinda weird on my setup right now. I can't
    step I can't really anything.

[33mcommit e95c9926eef96aae6a36856874c0770dbc3c25f5[m
Author: Henrik Böving <hargonix@gmail.com>
Date:   Tue Mar 31 19:55:34 2020 +0200

    Adding support for lots of Holtek chips
    
    I only tested HT32F523xx chips myself, all others are untested and
    should be considered a source of trouble when working with these chips.

[33mcommit 88a56e566f6496f995a1c617b33df8b46c7dee84[m[33m ([m[1;33mtag: v0.6.0[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 30 21:23:40 2020 +0200

    Address review

[33mcommit 33b68e8772d7cbf098d019bec9a4d2f5e9256e00[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Mar 30 21:22:36 2020 +0200

    Update README.md
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit 5df0db04076e4667d8dabf9078863f6b7f2a5519[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 30 20:27:24 2020 +0200

    Address feedback

[33mcommit dacdd122814fbebb0a8bfce9420813b26987c0a1[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Mar 30 20:09:46 2020 +0200

    Update README.md
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit d4dd5adacfe46ab23a1202b4a7c94696ecd1c8b8[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Mar 30 20:08:36 2020 +0200

    Update README.md
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit 19c621194b368999f92b4dde100587fd9c5505c0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 29 20:00:55 2020 +0200

    Address feedback

[33mcommit 430d57ba1faa35619db5145baebe333f256ef9d6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Mar 27 17:22:51 2020 +0100

    Add sponsors section

[33mcommit dcb876936e44fef53de2d94731b311cbdf24341c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Mar 27 14:54:56 2020 +0100

    Update README

[33mcommit e62b21684a4a812c33edc24ac9fab30cfc1fd768[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 30 15:58:05 2020 +0200

    Update the CHANGELOG for 0.6.0

[33mcommit f7c7febe7f9ae550d82cc13cb818109f09ca4ffe[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 29 14:37:07 2020 +0200

    Ensure unsupported protocols are not used

[33mcommit 702ca8e46ee576fb761bd3fe9fe04089cbd21968[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Mar 24 22:36:40 2020 +0100

    Ensure probe can be used if autodetect fails
    
    When creating an interface fails, we should return the probe again.
    Otherwise, the probe becomes unusable.

[33mcommit 8a1c736a343a5721691aabb26215be7d2e900144[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 29 18:05:19 2020 +0200

    Actually use apt update for all builds, also update checkout action

[33mcommit fe03fc4cf6147bb7e59255a588e4e8db42b4431d[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 29 17:35:39 2020 +0200

    Update apt information before install
    
    We're seeing 404 errors when trying to install packages. Hopefully
    this improves that.

[33mcommit d77de28d65e3dd2d524d92cddba8f3eca9a2016c[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Mar 29 20:11:38 2020 +0300

    Add change log items

[33mcommit e0f82c694e6aa71db06e9b07c5b38d1231c490d7[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Mar 29 19:30:20 2020 +0300

    Mention spec chapter

[33mcommit 67fb34c0672fe8413efca218ebfc9340faaccc3b[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sat Mar 28 01:06:55 2020 +0300

    Handle ADIMemoryInterface initialization errors

[33mcommit 4e6ba65cefffd3a8fa978d36d4f25a40943f6df3[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Fri Mar 27 23:48:15 2020 +0300

    Add doc-comment

[33mcommit 05511d0b73879f8cd562bd875dca1dc7ce1a9a18[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Fri Mar 27 23:37:52 2020 +0300

    Implement true 8-bit read/write

[33mcommit 5b4539dcf732b24e6bc1aa6876bea8bade7ddcc3[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Fri Mar 27 23:33:55 2020 +0300

    Check support for non-32bit data size

[33mcommit aca337850a4dc4a64afcf356db87f7febb5ce30b[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Fri Mar 27 20:04:24 2020 +0300

    Fix MockMemoryAP

[33mcommit 435be3778242b6829ce9f1dcc7f7f30736aa3600[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Fri Mar 27 18:49:00 2020 +0300

    Fix docs for the 8-bit access functions

[33mcommit 31977737d7051e446089e952ee0ede7b09ebc4cd[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Mar 27 16:53:13 2020 +0100

    Return an error when the ELF has unexpected contents

[33mcommit 5ae850b7ff4632d60d432aa08888f8de81b1956f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Mar 27 16:22:15 2020 +0100

    Improve logging of ELF loading

[33mcommit e4abf705f124c95d4e6825d1ee5f6cb612c3fe3c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 29 15:47:48 2020 +0200

    Fix inverted ifs

[33mcommit 342565f58e3903b085db70ef9b198491053e80f1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 29 15:32:02 2020 +0200

    Address feedback

[33mcommit e9d2b3bcb91cca6cae622f97936d2bc9a3780e20[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Mar 25 22:54:55 2020 +0100

    Fix warnings

[33mcommit eb9352986b9a647d677520a13341824a8a12decc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Mar 25 22:40:20 2020 +0100

    Fix double attach which could prove difficult for certain probes

[33mcommit b23383f6d87fd5c89fcd422404dd8393ad67b3ad[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Mar 21 14:28:03 2020 +0100

    Preparations

[33mcommit 6f3b09c5293d9fc94685b5e0392e86dac53d178f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Mar 23 21:27:05 2020 +0100

    Update object crate, remove capstone from probe-rs

[33mcommit aa493e04ce376f695b8d76ae08195585b109b208[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Mar 24 22:55:14 2020 +0100

    Flash algo fix (#181)
    
    * Improve error messages for register read & write operations for the ARM architecture
    
    * Fix #100
    
    * Clean up error mess for flashing; Fix #17
    
    * Break structure open that forces a page to be a sub-unit of a sector
    
    * Rename flash -> flashing module
    
    * Remove old expects
    
    * Add flash layout visualization for the flash builder
    
    * Fix flash page fill algo; fix public API and add comments
    
    * Add an option to restore unwritten bytes
    
    * Update docs
    
    * Add changes to changelog
    
    Co-authored-by: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit 8a042e5bbedc0e74e7c6b98653ce2db5f5ef3d8c[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Mon Mar 23 23:25:04 2020 +0300

    Change default JLink protocol to SWD

[33mcommit ddd4a970395fa50e3a9e202bcd1c3f3641a04270[m
Author: Emil Fresk <emil.fresk@gmail.com>
Date:   Mon Mar 23 19:38:52 2020 +0100

    Improved docs, added debug, changed to use duration is the events
    
    Rustfmt fix

[33mcommit 094fd82e70b90b6036b4f9334541a3999d592b5f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 22 17:07:06 2020 +0100

    Update changelog, increase version of probe-rs-t2rust to 0.6.0

[33mcommit 8a61f3d22e37bff9fee0fea2928c2e73a1c357f3[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 22 10:40:47 2020 +0100

    Replace owned Strings and Vectors with Cow
    
    The auto-generated code for the target definitions contained a lot
    of Strings and Vectors. For the generated targets, all this data is
    constant, and does not have to be owned. This commit replaces all
    Strings and Vectors with Cow pointers, meaning that they are now all
    statically allocated, but can still be changed if necessary.
    
    This also improves the compile time of probe-rs, it takes now about
    half as long to compile as before on my machine.

[33mcommit bfaf10e0363ef1eeba6f9674e60f9d2977860ccb[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Mar 16 23:17:28 2020 +0100

    Fix building without builtin targets

[33mcommit f1484ded6cfb1c752315ed98278a7b1a36412660[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 17:01:44 2020 +0200

    Fix clippy lints and a typo

[33mcommit e0c63c61ee817739f7385dca71a29369c5d42ce6[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 14:20:34 2020 +0200

    Use pread/pwrite for nicer code

[33mcommit 17f4db4b076148a8c1a9f700e1f046aae2ef98a1[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 10:41:57 2020 +0200

    Update changelog

[33mcommit 1bb86c7c76d6bfd63d56cb231bd7cd5974c114bd[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 10:35:15 2020 +0200

    Rewrite tests for adi_v5_memory_interface
    
    All tests now test various address(/length) combinations to ensure that
    all edge cases are covered. Additionally the write tests also make sure
    that the writes do not clobber any adjacent bytes.

[33mcommit 3c9b388850773b11cec34e5be9eb0ad14c2b9e32[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 08:38:48 2020 +0200

    Simplify 8-bit access in adi_v5_memory_interface
    
    Refactored out all the address alignment code, and replaced some of the
    clever byte handling with a simple extra Vec. Also reduced the number of
    writes in write_block8 to one.

[33mcommit b50236d3a282a82772fe940ddc9a0d05a382814e[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Sun Mar 15 18:00:14 2020 +0200

    Fix math errors in read_block8

[33mcommit 271dfa18ffbc2c9c1f8dde1addea64dac6b540b1[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Mar 17 14:36:45 2020 +0200

    Specify endianness for pread/pwrite everywhere

[33mcommit 24ef64597dd9572806f6a505e9d508e1a0864b99[m
Merge: 9757548 4df0919
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Mar 16 22:05:28 2020 +0100

    Merge pull request #178 from probe-rs/0.5.2
    
    Prepare for 0.5.2

[33mcommit 97575483c9ddd694f09e9b48fb8b44802f44b8e7[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Mar 16 21:54:42 2020 +0100

    Add missing changelog entry for ST-Link fix

[33mcommit ad33c5e8898a039f6becfd9c815bf24cef25ca08[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 15 23:11:02 2020 +0100

    Remove types for DebugPort versions, improve errors

[33mcommit fdf3ec87eab94f87087a12f9b3728dd593011238[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 15 16:09:00 2020 +0100

    Fix tests

[33mcommit 22b69ab230bcd669121289a94b243b391fe13b05[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 15 15:49:26 2020 +0100

    Fix memory writes for ARM platforms
    
    When using a Memory AP to write data into memory, we only
    wrote the value into the 'DRW' register of the AP.
    
    The problem is that this does not mean that the value will
    be actually written into memory. The SWD protocol requires
    that the host does continue to clock the SWD clock after
    writing data, to ensure that the SWD transfer can be
    completed. In the case of the ST-Link, it seems that
    the probe itself does not do this, as least in the
    way it is used by probe-rs right now. To ensure that
    all transfers can be completed, I have added a read
    of the RdBuff register after all writes.

[33mcommit 4df0919f03475196ded135c5543e00527f8c65f2[m
Merge: 5c88e14 c97b681
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Mar 12 11:32:07 2020 +0100

    Merge branch 'master' into 0.5.2

[33mcommit c97b681abd8be72bf91bb9dc17dd4df50e8038da[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Mar 11 23:52:21 2020 +0100

    Update CHANGELOG.md

[33mcommit 5180e559f034f59e6155851fa45d12add7b45a92[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 10 23:56:53 2020 +0100

    Update changelog

[33mcommit 8253fd006b64e47295f64c5a3758f1543ba46afb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 10 23:53:10 2020 +0100

    Fix parsing of yaml files on the go when probe-rs is already running (not compile time)

[33mcommit 5c88e14ef37470cfcbfdd0d8c933f5a77d71e9a8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 10 22:47:51 2020 +0100

    Bump Cargo.toml version

[33mcommit d0e69edf6bec92f9810f51388de35843755bade8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 10 22:42:37 2020 +0100

    Prepare for 0.5.2

[33mcommit dac11993e56fb77b496ce94a8e6fcb14e1a6c016[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Mar 10 22:28:15 2020 +0100

    Ensure the configured speed is always used for the STLink

[33mcommit a39f7b64f323ea5626e6718d0a3d180563153613[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Mar 10 20:25:35 2020 +0300

    Implement speed/set_speed methods for JLink

[33mcommit 10d8c6ff9fb6c73b24506994f3babfddb0fda609[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Mar 10 18:17:03 2020 +0200

    Run rustfmt

[33mcommit c4554aa55c1ca8e6ce29c340fa34b8db599ce687[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Mar 10 18:12:30 2020 +0200

    Fix typo

[33mcommit b9b1d5166fb8f94405127ed94477b220a2c2bc5d[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Tue Mar 10 18:12:23 2020 +0200

    Add stlink v3 support

[33mcommit 699c661263195663c0998e5737c37b93f4f359d0[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Mon Mar 9 01:30:09 2020 +0200

    Implement speed/set_speed methods for STLink

[33mcommit 74a1e1c340e1553dea79105201939f0e44a8cc5c[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Mar 8 20:03:22 2020 +0200

    Implement speed/set_speed methods for DAPLink

[33mcommit 14c06a3c32fd5e4bb7d0d8eb577e46046f5067bb[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Mar 8 18:10:21 2020 +0200

    Add speed/set_speed methods and error kind

[33mcommit c95aa57f0136b2386197bc26f07522cbe422d53a[m
Author: Emil Fresk <emil.fresk@gmail.com>
Date:   Tue Mar 10 09:05:06 2020 +0100

    Updated changelog

[33mcommit 74a9c343d655e0ad9983a205bff335840b4ac03e[m
Author: Emil Fresk <emil.fresk@gmail.com>
Date:   Mon Mar 9 21:23:49 2020 +0100

    STM32L4 target

[33mcommit 35a32a01f929ae7b0724072efd1c32f7fc800fee[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue Mar 10 07:50:01 2020 +0000

    Update derivative requirement from 1.0.3 to 2.0.0 in /probe-rs
    
    Updates the requirements on [derivative](https://github.com/mcarton/rust-derivative) to permit the latest version.
    - [Release notes](https://github.com/mcarton/rust-derivative/releases)
    - [Changelog](https://github.com/mcarton/rust-derivative/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/mcarton/rust-derivative/commits)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit fcfae794a39ce5842e3fdebadc6a090c213337ba[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Mon Mar 9 08:13:07 2020 +0000

    Update base64 requirement from 0.11.0 to 0.12.0 in /probe-rs-t2rust
    
    Updates the requirements on [base64](https://github.com/marshallpierce/rust-base64) to permit the latest version.
    - [Release notes](https://github.com/marshallpierce/rust-base64/releases)
    - [Changelog](https://github.com/marshallpierce/rust-base64/blob/master/RELEASE-NOTES.md)
    - [Commits](https://github.com/marshallpierce/rust-base64/compare/v0.11.0...v0.12.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit d6cade0e820ca721275224af76168c7b8ce313ba[m[33m ([m[1;33mtag: v0.5.1[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 8 21:01:38 2020 +0100

    Removed unnecessary titles

[33mcommit 4033cd6cb83ea06ee5bcd3884afc80af95f8c372[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Mar 7 23:24:09 2020 +0100

    Update changelog

[33mcommit eca8c2baff91beb2ed5675535fa1056f4777e41f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Mar 7 23:22:27 2020 +0100

    Bump version

[33mcommit a9768c1b0f6f69e0918fde228d87b22f80789acb[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 8 17:44:48 2020 +0100

    Improve error handling for flashing
    
    Add additional states to indicate when flashing
    or programming failed, and ensure we don't wait
    forever for flash loader functions to finish.

[33mcommit 92ce82b4530f346c153d20b68d1bb67f622d2dfb[m
Author: Vadim Kaushan <admin@disasm.info>
Date:   Sun Mar 8 10:12:00 2020 +0200

    Fix checks for STLINK-V3

[33mcommit 00572c73377cefe1307b03ecc9e2774dcea368d5[m
Merge: 8beedf6 030114f
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat Mar 7 23:19:33 2020 +0100

    Merge pull request #167 from probe-rs/fix-m3
    
    Add the M3 string resolution

[33mcommit 030114fdc6b107c21ee2207c50332ff52cab2dad[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Mar 6 11:59:27 2020 +0100

    Add the M3 string resolution

[33mcommit 8beedf697582c8959187e600c1533558b4dfa259[m[33m ([m[1;33mtag: v0.5.0[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 3 22:12:08 2020 +0100

    Prepare release

[33mcommit 34972c724ea670de0a0670a861bf4f5de2917246[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Mar 3 21:52:46 2020 +0100

    Rename target selection option to --chip

[33mcommit 57a1d18a70f9e84e94c71264bc256c0fe1c2a1ef[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Mar 2 01:41:39 2020 +0100

    Remove outdated material

[33mcommit bd333b1f02f5b00b919b72c070ee16dc81b8d4c9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 1 22:29:27 2020 +0100

    make dependencies for gdb-server crate not required

[33mcommit 23e8438acfb1c8df7f0044cdcd7a14ee8e7d6935[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 1 21:52:00 2020 +0100

    Fix deprecation warnings for hidapi and change jaylink to released version

[33mcommit 224883268875e588fd33e6baa64c560dd5f3f8d2[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Sun Mar 1 20:59:55 2020 +0000

    Update pretty_env_logger requirement from 0.3.0 to 0.4.0 in /gdb-server
    
    Updates the requirements on [pretty_env_logger](https://github.com/seanmonstar/pretty-env-logger) to permit the latest version.
    - [Release notes](https://github.com/seanmonstar/pretty-env-logger/releases)
    - [Commits](https://github.com/seanmonstar/pretty-env-logger/compare/v0.3.0...v0.4.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit fff50190eab4973daa75c8a84a3425cb2d266c11[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 1 21:48:12 2020 +0100

    Add file based dependabot

[33mcommit d90341bd607967f9728561fa23f607fb88c04e3d[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 1 21:33:18 2020 +0100

    Revert "Fix page count calculation"
    
    This reverts commit aaf2afc3f768c54499db4b6a4ea71544e95b58f7.

[33mcommit 4b1b40ffec24863ab7d7da7371319f30e8fc1582[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 1 20:52:46 2020 +0100

    Fix typo in changelog

[33mcommit b5a634bd42e73446afaaeecfe4a17e0a06eef106[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 1 20:49:11 2020 +0100

    Run rustfmt

[33mcommit f70e27a2fde01837e84ab699461459e85a01b1b5[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Mar 1 20:47:22 2020 +0100

    Update probe-rs/src/probe/stlink/mod.rs

[33mcommit b9bb703ae89ad57bfa85419097cc8b2b48d50590[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Mar 1 20:38:08 2020 +0100

    Clean up some code

[33mcommit 06f1194ccfda1161b684fc63732f2aa6f0e08c9b[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Mar 1 20:15:24 2020 +0100

    Update changelog

[33mcommit 00db329e0f555b7f034af679edeeec2582938ed8[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Feb 29 17:37:32 2020 +0100

    Add support for stm32wb55, improve support for multiple APs

[33mcommit de7b0ca5f3e89009e64d739203181985a5a7e757[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 21:21:20 2020 +0100

    Add FromStr for WireProtocol

[33mcommit c21111472683529f3a8084548e855c3163414b1e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 20:00:19 2020 +0100

    Remove allow of dead code again

[33mcommit 0dd4273d52d24a3f6d6045806124d437cc122e50[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 19:30:15 2020 +0100

    Cleaned up the gdb stub with hopes of cleaning cognitive complexity; didn't help as expected ...

[33mcommit 613627bdcf91667faaedaee2fa37a80b335ab25a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 18:19:41 2020 +0100

    Fix all warnings except for the cog complexity for now

[33mcommit 9f672221825ec0b2e3a62c61a4e2ff5ebf6c0d5d[m
Author: Danilo Bargen <mail@dbrgn.ch>
Date:   Mon Feb 24 22:30:34 2020 +0100

    Add STM32L0 target
    
    Generated with target-gen using
    https://keilpack.azureedge.net/pack/Keil.STM32L0xx_DFP.2.0.1.pack

[33mcommit aaf2afc3f768c54499db4b6a4ea71544e95b58f7[m
Author: Danilo Bargen <mail@dbrgn.ch>
Date:   Tue Feb 25 22:32:14 2020 +0100

    Fix page count calculation

[33mcommit 411bff170dad200cbf8440c638651b17cacb219b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 17:20:47 2020 +0100

    Address further review

[33mcommit 6c74ff75cdf9136d77120533a0148defac910b8b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 14:38:29 2020 +0100

    Format code

[33mcommit 477780d929a5b42e475b682195087e1553b58b5a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 29 14:33:48 2020 +0100

    Address feedback:
    - Generalize ADI init code and move it to the ArmCommunicationInterface
    struct
    - Clean up erroring
    - Clean up DP access

[33mcommit 07e3c2c27f4b9cba2e08cdf5a1fcc11ef6017308[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sat Feb 29 11:48:32 2020 +0100

    Update probe-rs/src/probe/jlink/mod.rs
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit d0cf5dbd8d4c17e32ff2550b5b4b3d3c6bd1a052[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 28 23:19:34 2020 +0100

    Add SWD support for JLink

[33mcommit 644ad1a42c603a2475f9a89a0a3a71802db52749[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Feb 28 22:04:47 2020 +0100

    Remove files added by mistake

[33mcommit 749b85cbc086af6e3186a06eaa5ce35b26ba3119[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Feb 28 21:56:13 2020 +0100

    Add timeout to

[33mcommit 3d0d2c7eb805616a2a89b3b48c5ab6c877ebce6e[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Feb 28 21:28:27 2020 +0100

    Update CHANGELOG

[33mcommit 11ff995084b73847a990603340b6f8f867774ba8[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Feb 28 19:20:10 2020 +0100

    Reset errors when connecting to RISCV debug module

[33mcommit 9b54c33a8815914b8745c1907c031f3a35589ea2[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 26 23:57:22 2020 +0100

    Don't panic when closing the interface fails

[33mcommit 2aa62623c19c02d3d2a48d27fe3c2a4754f85b42[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 26 22:57:57 2020 +0100

    Address review feedback

[33mcommit a777d78302b327208b1802875c7bad024811f5c0[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 26 19:21:24 2020 +0100

    Update probe-rs/src/architecture/riscv/mod.rs
    
    Co-Authored-By: Yatekii <Yatekii@users.noreply.github.com>

[33mcommit 64b23a5c5e3beea73134d050efeecca1a7022d9a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 26 19:21:12 2020 +0100

    Update probe-rs/src/architecture/riscv/communication_interface.rs
    
    Co-Authored-By: Yatekii <Yatekii@users.noreply.github.com>

[33mcommit 075c960c0e405093e2d9fa8e8b926cc5aa16162e[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 26 19:20:15 2020 +0100

    Apply suggestions from code review
    
    Co-Authored-By: Yatekii <Yatekii@users.noreply.github.com>

[33mcommit 94e909b384e72c25f656c7287b55ebcb8760655c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Feb 24 16:15:07 2020 +0100

    Add register support for RISCV

[33mcommit cfe28963e0805e1c27227837a32884e26d265d41[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 19 18:18:22 2020 +0100

    Fix breakpoints for RISCV

[33mcommit 9a2bf6a058af7ab38205a664056fa50fc7de9020[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 18 23:47:00 2020 +0100

    Cleanup JLink implementation, remove RISCV specific parts

[33mcommit 8aada798a0f9fe1677508d14ccdb6b64fe91151a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 18 23:02:36 2020 +0100

    Remove unused code and dependencies

[33mcommit 40d655aacc2fd8ed5a69af00f9eb8ed0315e8904[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 18 20:16:37 2020 +0100

    Initial support for breakpoints on RISCV

[33mcommit 634ef0dce32fd1ad332fb7a9201708a45bcb7cbf[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 18 17:09:10 2020 +0100

    Correct instruction for writing CSR

[33mcommit 72ca2373cea0eed19128ec6a61368538f4f73a68[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Feb 17 23:49:10 2020 +0100

    Working reset for RISCV

[33mcommit 41d9473f5cb2b816f43bb34af9cfedf4e508f2a2[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sun Feb 16 11:00:39 2020 +0100

    Add support for step and resume

[33mcommit fd47dde0616d5c5b310a5b5568c47b59c2df62e6[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Feb 14 00:38:27 2020 +0100

    RISCV Memory Access, using progbuf

[33mcommit 428556a8254cdc7bbff721c7d202e47c01a111c9[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Feb 13 19:31:14 2020 +0100

    Working memory read on RISCV

[33mcommit 402368ea103153cccf7472e263b6e7ee234ec287[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 11 23:41:56 2020 +0100

    Use Github version of jaylink

[33mcommit 78dc3e2d08e117c798b3ff04be85234993e12445[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Feb 11 23:37:38 2020 +0100

    WIP: Riscv halt works with new API

[33mcommit 5133cbc974d335fb3432a9a3d2e776901ecb8cb4[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Feb 2 22:53:30 2020 +0100

    WIP: Working halt for RISCV

[33mcommit 3c750bdbe9011b037f506c3529d1182b75d866f9[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Feb 2 14:07:13 2020 +0100

    WIP: Halt support for RISCV

[33mcommit c4e40d0f6f84e635c8cf4b86ac7e4498a87618d4[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Jan 27 21:38:30 2020 +0100

    Initial support for J-Link and RISC-V

[33mcommit d63d4f8de1ffe899ab34cf65eb3a40a3181ca0d6[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Feb 25 17:55:59 2020 +0100

    Update probe-rs/src/core/mod.rs

[33mcommit e0b7959dd8a81290c442b4685d71d8b33d6554f8[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Feb 25 17:54:59 2020 +0100

    Update probe-rs/src/core/mod.rs

[33mcommit 31ac11def5dd6898b9d2776ea6bb6a5db7093f29[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Feb 25 17:54:48 2020 +0100

    Update probe-rs/src/core/mod.rs

[33mcommit 7ee252b55cb358d5b15ff2714389833a723f668f[m
Author: Matti Virkkunen <mvirkkunen@gmail.com>
Date:   Tue Feb 25 14:48:59 2020 +0200

    Add Cortex-M3 support by re-using Cortex-M4

[33mcommit e8a1f8eede27b5860391e063618532217e6a2fa0[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Fri Feb 21 13:36:49 2020 +0100

    Fix clippy lint about clone

[33mcommit 16aac92f087455be4a666d3a277a31c7391a0a84[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Fri Feb 21 11:55:12 2020 +0100

    Use static register description, add non-panicking methods for register access

[33mcommit 9aad359a2f11a0e6f928281bad0d6d8caeaa797e[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 19 20:19:01 2020 +0100

    Initial implementation of new register description

[33mcommit 00e45a542b65b712fa7738ee0dd0aae7b0e6be06[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Fri Feb 21 14:51:41 2020 +0100

    Add support for DWARF requires-register evaluation

[33mcommit a6a33629c677b80cd097ae6125aef4bbe0ee0e22[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Feb 10 20:37:16 2020 +0100

    Address review feedback

[33mcommit ea5f3f13a02c9fbf90c3dd2dd0305600b3580824[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sun Feb 9 17:06:18 2020 +0100

    Cleanup debug erros, make some functions private

[33mcommit 11da80735feab62ea8e95b084caa73b3bc2d538d[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Feb 5 21:10:44 2020 +0100

    Add log output when switching ST-Link mode

[33mcommit da0ef9a5a3ac68c02d832c267cd5cba1a7679073[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Nov 18 22:05:11 2019 +0100

    Improve location search for breakpoints

[33mcommit 0e385617e2a9658c1b7c4a30e02db6449a42d7d7[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 30 19:51:56 2019 +0100

    Fix clippy lints, update structopt

[33mcommit 5cd82d45e14a85238785f162686ea6c29026c07a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Sep 25 21:44:54 2019 +0200

    Add function to get PC for breakpoint from source location

[33mcommit 3e302cb84706a37ecf31d689bb86a9b5af5508fd[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Sat Feb 8 09:51:35 2020 +0000

    Update pretty_env_logger requirement from 0.3.0 to 0.4.0 in /cli
    
    Updates the requirements on [pretty_env_logger](https://github.com/seanmonstar/pretty-env-logger) to permit the latest version.
    - [Release notes](https://github.com/seanmonstar/pretty-env-logger/releases)
    - [Commits](https://github.com/seanmonstar/pretty-env-logger/compare/v0.3.0...v0.4.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 00d173c581a4e45b40d60856c51fa64cf07847a2[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Sat Feb 8 09:51:14 2020 +0000

    Update rustyline requirement from 5.0.2 to 6.0.0 in /cli
    
    Updates the requirements on [rustyline](https://github.com/kkawakam/rustyline) to permit the latest version.
    - [Release notes](https://github.com/kkawakam/rustyline/releases)
    - [Commits](https://github.com/kkawakam/rustyline/compare/v5.0.2...v6.0.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 05f03c7abee584293bee5a615b942be58d858e3b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 22:39:57 2020 +0100

    Run cargo fmt

[33mcommit f5522bcdf4a108e24e28e3c54617a8f3a2c67854[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 22:39:21 2020 +0100

    Clean dependencies and update readme

[33mcommit 310673e13ef4d0f4d4ff1b62c6660a8acc4014e0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 22:10:22 2020 +0100

    bump versions to 0.5.0 so we don't have conflicts with users

[33mcommit 999d255914d4d407e8b5aa25dd683f83ad91b428[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 22:07:06 2020 +0100

    Update readme with examples

[33mcommit d258b6b5121575b134680ea297f9c97dfc8237bd[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 22:02:39 2020 +0100

    Update changelog

[33mcommit 470f002211b01f6feb0b5f710285b17c2c7530a3[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 21:52:54 2020 +0100

    Clean exports a little (config module)

[33mcommit 94f120768fc4aec95117299c6c4dc5312751e43e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 19:55:50 2020 +0100

    Make families of the registry publicly available

[33mcommit 8ef5cf1e1e33e7bb1a18cdf826db81a034da42ff[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 19:37:00 2020 +0100

    Internalize registry

[33mcommit aee550b738061670562e95feb23095b8e5ace2be[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Feb 7 09:39:05 2020 +0100

    run cargo fmt

[33mcommit 85522b5ec80b144da91a8e166e18f525776bbe3c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Feb 6 02:28:58 2020 +0100

    Add a From<()> for TargetSelector impl for atutomatic target selection. Also adapt ram_download example to reflect the core/memory changes

[33mcommit 851ed37434f49a72ca3d4e0958a7f55ce589c88c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Feb 6 02:20:26 2020 +0100

    Unify SelectionStrategy & TargetSelector into TargetSelector; this essentially reenables autodetection; also ran cargo fmt

[33mcommit ed738087a07bb78401ed0299cfe9d638b6a9d10f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Feb 6 02:00:52 2020 +0100

    probe-rs should support autodetect again

[33mcommit 8c24690eeedf408879768ee74eb978ac5041cfd9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Feb 5 23:08:17 2020 +0100

    Adjust flashing API to new API

[33mcommit 725d8598e0e418b3dc1e89a855c73069d0f55eaa[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Feb 5 22:33:13 2020 +0100

    Remove cargo flash for good

[33mcommit 9a111b6dc31334c91dd18af9183b56d27c188b4a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Feb 5 22:32:09 2020 +0100

    Add memory reading to the core and extend examples

[33mcommit fcfc26c3c9ff19ecf269945b1c97a2ed015a9255[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Feb 4 23:28:45 2020 +0100

    Get minimal example working

[33mcommit 561a22a534878f30ee18fbf8a51b3b6e184acde4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Feb 4 21:55:54 2020 +0100

    Run cargo fmt and clean most warnings

[33mcommit 6e37b10b9212b77e6503446390905e60bd538bcc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Feb 4 21:47:17 2020 +0100

    Cli builds

[33mcommit 95a0a426472846d5fcfdefa33e89496c96075cdc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jan 21 22:48:27 2020 +0100

    Rename MasterProbe to Probe
    
    Move stlink error -> debugprobe error conversion to the stlink module
    
    Add the MemoryInterface trait and the Memory struct and adapt the DebugProbe trait
    
    WIP new api; DOES NOT COMPILE; amend later!
    - Main API is done
    - Errors are kinda cleaned up
    - Binaries do not compile yet, lib is done
    - Completely untested
    
    Broken types ...
    
    Main lib compiles again. Tests run. ram_download example fails tho.
    
    Add eprintln of read & written data to ram_download example
    
    cargo flash as well as examples work. the GDB server implementation is 100% unsound. needs to be made thread-unsafe!
    
    Remove all unsafe; Make gdb server sound again; to be tested
    
    Clean up warnings and run cargo fmt
    
    CLI compiles again and seems to work
    
    Stared reorganizing modules for better useability; also started docs; highly unfinished; DOES NOT COMPILE; maybe amend later
    
    Cleaned up probe-rs structure a lot; binaries do not compile; lib does
    
    Restructuring done for now; Docs will tell if we need more
    
    WORK; highly unfinished; ammend later

[33mcommit a9b7d26fa4ecd91a96046a6c9c41d23b9e671c52[m
Author: Damjan Georgievski <gdamjan@gmail.com>
Date:   Thu Feb 6 19:21:26 2020 +0100

    fixed by target-get PR 11

[33mcommit 7dff17dd8b7dcaf2fca9ecc722b501c0f6c77e2d[m
Author: Damjan Georgievski <gdamjan@gmail.com>
Date:   Thu Feb 6 15:03:59 2020 +0100

    add STM32G0 series
    
    the 'STM32G0 Series.yaml' file is created with `target-gen` from the
    'Keil.STM32G0xx_DFP.1.2.0.pack' downloaded from
    https://www.keil.com/dd2/pack/

[33mcommit b706c2bd6152c6d118ec408a6d279b85624adf79[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Thu Jan 30 07:37:10 2020 +0000

    Update pretty_env_logger requirement from 0.3.0 to 0.4.0 in /probe-rs
    
    Updates the requirements on [pretty_env_logger](https://github.com/seanmonstar/pretty-env-logger) to permit the latest version.
    - [Release notes](https://github.com/seanmonstar/pretty-env-logger/releases)
    - [Commits](https://github.com/seanmonstar/pretty-env-logger/compare/v0.3.0...v0.4.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 2a02edc4b6279e999276e1ae5e212b2681d63800[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue Jan 28 12:36:43 2020 +0000

    Update gimli requirement from 0.19.0 to 0.20.0 in /probe-rs
    
    Updates the requirements on [gimli](https://github.com/gimli-rs/gimli) to permit the latest version.
    - [Release notes](https://github.com/gimli-rs/gimli/releases)
    - [Changelog](https://github.com/gimli-rs/gimli/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/gimli-rs/gimli/compare/0.19.0...0.20.0)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 104b31ac922e82ce154df0cdff01d713a3118728[m
Author: dependabot-preview[bot] <27856297+dependabot-preview[bot]@users.noreply.github.com>
Date:   Tue Jan 28 12:37:05 2020 +0000

    Update goblin requirement from 0.1.3 to 0.2.0 in /probe-rs
    
    Updates the requirements on [goblin](https://github.com/m4b/goblin) to permit the latest version.
    - [Release notes](https://github.com/m4b/goblin/releases)
    - [Changelog](https://github.com/m4b/goblin/blob/master/CHANGELOG.md)
    - [Commits](https://github.com/m4b/goblin/commits)
    
    Signed-off-by: dependabot-preview[bot] <support@dependabot.com>

[33mcommit 1e0cfef378dc7e727f3a3a4f0561cfa7a91c013f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jan 29 21:16:28 2020 +0100

    Move cargo-flash to separate repository

[33mcommit 149e4375baea473bec4e718b9ed28e0d10985a4a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Jan 23 17:01:37 2020 +0100

    More consistent naming of errors

[33mcommit 113c5bf5b6a07ddaf8e4a2423379e1370d75cdc8[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jan 22 22:10:48 2020 +0100

    Refactor daplink specific errors

[33mcommit c29717ea264d9f7af9873b8cfaf6c8ad34cf57d9[m
Author: David Sawatzke <d-git@sawatzke.dev>
Date:   Wed Jan 22 17:55:22 2020 +0100

    Add stm32f0 support

[33mcommit 992547580a8c4a973c84d3d3f33b908d083f8818[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jan 21 22:32:10 2020 +0100

    Add PR template

[33mcommit 0e5b2082d2088a12d92bd54f1f8987094e47b49e[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Jan 21 20:02:14 2020 +0100

    Refactor probe error handling, especially regarding st-link
    
    Move ST-Link specific errors to a ST-Link specific error type,
    to clean up the DebugProbeError type.
    
    The rental crate is also removed, to simply the implementation of
    the ST-Link support.

[33mcommit 281c9b2c3425df316991daf95891a2645be9a449[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Jan 16 22:46:43 2020 +0100

    Fix wildcard dep

[33mcommit e67651739a1f3d732b529a3cf68e39fb3d154b92[m[33m ([m[1;33mtag: v0.4.0[m[33m)[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Jan 16 00:24:46 2020 +0100

    Update CHANGELOG.md

[33mcommit bc64137a955a76703e2cad7f9e04edf82afe35b8[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Jan 16 00:22:43 2020 +0100

    0.4.0 release (#126)
    
    * Make crates ready for release

[33mcommit 4d2365eddd4cc0a5b3bb7b740ccaea230b989192[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 10 21:29:22 2020 +0100

    Fix a bug in the write_block8() method to make tests run again

[33mcommit aed4ee8581d3982a928513756ed9844a11f2992c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 10 21:03:52 2020 +0100

    remove accidential main.rs

[33mcommit 41d0d0c1f38f1673972b17512912d588e99a608f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 15 21:32:02 2020 +0100

    Fix most of clippy lints and run cargo fmt

[33mcommit 750611be07b2db704d6f117885e643ee896a9ee8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 00:30:32 2020 +0100

    Remove printlnsqlite3 db.sqlite3

[33mcommit 253335178f1aca911f762f1af40e9a352fc3e968[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 00:23:34 2020 +0100

    Implement mon reset

[33mcommit 2979cb6a377269906efdd918d6a7c64632a03ea0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 00:10:46 2020 +0100

    Made the GDB stub more foolproof

[33mcommit 7060e1d8370335b6e08673eff9935e7c0aea21af[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 23:32:25 2020 +0100

    Relocate the gdb-server machinery into a lib/bin crate which features a binary to open a simple GDB server as well as a lib which enables other crates to open a server

[33mcommit 81a5e0276a1ed92f592b9c8e1fe0d3006a692606[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Jan 7 01:04:44 2020 +0100

    Fix the excessive CPU use after the TcpStream was lost

[33mcommit ab70656d234136e94f5cc2456371868dba40a342[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 15 21:29:07 2020 +0100

    Make si/s work

[33mcommit 736c39859bcbed503f86431e2f823d4ba6ee0e5b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 15 21:26:55 2020 +0100

    Add the gdb stub to cargo-flash
    --gdb flag for starting up the stub
    --no-download for preventing actual flashing if one just wants to
    attach
    --reset-halt to reset & halt the target after flashing
    --gdb-connection-string for specifying the address & port the stub runs
    on

[33mcommit e769211bd752cac8aef7356190fc21eee1d60974[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 5 23:30:41 2020 +0100

    interrupting works

[33mcommit f5174f43f5b133fb68b2d482b34cc4574ebd1968[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 23:29:41 2020 +0100

    GDB server compiles again; *should* work now

[33mcommit 0ff078181f859bc337795ca16ce6d830463fb6cf[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 3 02:05:27 2020 +0100

    Format

[33mcommit 157571ca1a3472a85ecf5a7488f01fa330430e00[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 3 01:11:43 2020 +0100

    UBER FAST NOW! Check for validity still

[33mcommit b1b408f336107c3e510ffabb70209bb90ef39751[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 3 00:57:18 2020 +0100

    This works but has weird lags and often does get a packet twice because it fails on the first try ...

[33mcommit 209d7e269a12861cdc1949bf0ba373590c4f34b4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 3 00:42:36 2020 +0100

    This version seems to work mostly

[33mcommit 3b1f9c611ed441cf24241745b6e5cb48c887dddb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Jan 2 23:16:11 2020 +0100

    Select variant. Does not really work :/

[33mcommit 040addab65150aa289d0e051d36b64024f23d95b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Jan 2 01:40:47 2020 +0100

    Async server runs but has serious bugs

[33mcommit 9fabecb95223a289e14e18ae47ea3891e523ce3a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 1 23:59:01 2020 +0100

    Async server runs but has some weird bug where it would just not ack and then try to ack forever

[33mcommit 25e2cbb0bdeefa042f1bbed89e52d78ea317920c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 1 01:09:28 2020 +0100

    WIP

[33mcommit 17e76d46c32a68adf048f7804af760f0501928d0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Jan 8 23:26:35 2020 +0100

    WIP

[33mcommit fccd8eaa5c93a93427b84726d4e6c2f50fd267b4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Dec 31 20:41:42 2019 +0100

    Fix cargo toml

[33mcommit f518e7134e87d030a08d194077d4dd16e22154f0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Dec 31 18:24:16 2019 +0100

    gdb finally uses HW bps

[33mcommit 5c1d559fe591a543a501fcfe22a99510a8b079d1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Dec 31 10:10:26 2019 +0100

    Progress

[33mcommit faf89687af2292ebea36e17269dc81fc730551c5[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Dec 30 18:08:52 2019 +0100

    hwbreak works

[33mcommit 35991362186dd1ebe5e9bff2b7712ac142016b11[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Dec 30 13:27:15 2019 +0100

    stepping nearly works

[33mcommit 5ebb987b7fb998fb3ec894196fb9ee309141a9cc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Dec 30 04:31:05 2019 +0100

    stepping mostly works; some kinks remain

[33mcommit 0e57b312d43ba71bcf6c615f63c773d0c3dcf767[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Dec 29 15:27:28 2019 +0100

    Getting a stacktrace in GDB works

[33mcommit 60a57fe83e09b603a3321d29f5ad10061295d25c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Dec 29 01:45:33 2019 +0100

    Start a minimal impl of a gdb server; does not properly work yet

[33mcommit addb9eac1618c34f96b5366acdfd557a726acbd8[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Jan 12 15:00:41 2020 +0100

    Only auto-select probe when a single probe is connected.

[33mcommit 592b4d5120b81194f0c0cadcaa9725139ec5f6a3[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Jan 12 14:41:14 2020 +0100

    Add functions to list all probes, and open probes from DebugProbeInfo

[33mcommit 29539c5658bf831f429a039bf4ecddd857131911[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 12 17:41:34 2020 +0100

    Add the previous targets again

[33mcommit 1dc0904382bff57a8818ba0add6d42ebc2666846[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 12 17:36:40 2020 +0100

    Fix tests

[33mcommit c414d0ad87221577066989820b86dcd10d4e26c9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 12 16:55:49 2020 +0100

    Make the flash blob in the yaml base64

[33mcommit 1e5ca0f22947cf1ede24c994c09f32edc4fb9858[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 12 15:59:27 2020 +0100

    Rename range to flash_range

[33mcommit 8c490478e221aecbb2e6c7bb630d7af978aab41f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Jan 12 09:07:34 2020 +0100

    Run rustfmt

[33mcommit 2131fad182187522416dfd15409cc543f5eb5457[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Jan 12 09:06:31 2020 +0100

    Fix size of erased sectors in progress bar

[33mcommit 5f17db9f1ff08caae521f6fc5caad454a9c0faa4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Jan 12 03:33:28 2020 +0100

    Use a HashMap for all the algorithms; Use a range for the flash properties

[33mcommit 711e4086e4af2d1d9a9d356a1ed03115e613eb0f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Jan 12 01:35:30 2020 +0100

    Fix flashing for STM32F4 (and maybe others)
    
    Add support for multiple flash algorithm per chip,
    and automatic selection of the right one.
    
    Add support for flashes with different sector sizes.

[33mcommit d30b209fe6ef78e2a2b994992990be8fa9fa6ba8[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sat Jan 11 19:39:31 2020 +0100

    Use address instead of count for sector definition

[33mcommit 08930a3caecb28ef77b81a7b9aecc4eaf4235cad[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jan 11 13:54:25 2020 +0100

    Initial support for flash with different sector sizes

[33mcommit 35180d2d0489125c3fd9214abfdee48ac090953d[m
Author: Per Lindgren <per.lindgren@ltu.se>
Date:   Sat Jan 11 19:55:23 2020 +0100

    usb PID added for Nucleo64 stm32f411re

[33mcommit 47e7ff8b876b4d56cdab10ed199cb649c3fffe18[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jan 11 12:30:04 2020 +0100

    Use only quote macro for target codegen

[33mcommit a0decb8f526965c9f4b46ab2773bbc6704e8e38b[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jan 11 11:21:58 2020 +0100

    Split code generation into separate crate
    
    Code generation is now done by the probe-rs-t2rust crate, which
    makes it easier to run it separately to inspect the output.
    
    The generated code is now also formatted with rustfmt, to improve
    error messages.

[33mcommit 0c56cbd40624a7979bb1d850fb9ed3c35b35bbcc[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jan 11 01:28:07 2020 +0100

    Check revision of FPBU

[33mcommit 11b871dea658133a82b7c5bb8f11fbc82ff7361f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jan 11 01:21:59 2020 +0100

    Fix breakpoint setting for Cortex-M4

[33mcommit 2dcc241568902a030d28cc29722046610882be9d[m
Merge: 8cde71c ac08a3d
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Fri Jan 10 23:38:22 2020 +0100

    Merge pull request #116 from probe-rs/no-progressbars
    
    Add a flag to disable progressbars

[33mcommit ac08a3d463b073fdfc172d8d248537012de74a2a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 10 22:42:36 2020 +0100

    Add option to disable progress-bars

[33mcommit c2f265ddd22ede1326412c6313c3aef1f7cf7a0c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jan 10 22:07:49 2020 +0100

    Disable progress bars

[33mcommit 8cde71cb8ee8bc0ce6d6ad736525098c6960612a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Jan 7 20:19:36 2020 +0100

    Fix formatting

[33mcommit 0b7a0d1fa15899a882d4d2238f6980927bad02cb[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Jan 7 20:15:38 2020 +0100

    Use addresses to clear breakpoints

[33mcommit 194496191ae20dfea7389c6af6461d65f700641b[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Jan 6 22:31:48 2020 +0100

    Fix clippy warnings

[33mcommit a21ffd1aed6e2e0fc1fe1a6fa80923c8a022daf5[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Jan 6 22:22:35 2020 +0100

    Add breakpoint support for Cortex-M33

[33mcommit d8932d1d51becce766301afa763a7ae8c75fa30c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jan 1 18:34:09 2020 +0100

    Add breakpoint support for Cortex-M4

[33mcommit 0797cc6c8f3d2a815ae99aba97c6a60266e48d87[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jan 1 16:49:22 2020 +0100

    Add support for multiple breakpoints on Cortex-M0

[33mcommit 544e3f7db0f275b3d80e71899694255eae693c87[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Jan 7 00:06:35 2020 +0100

    Update issue templates

[33mcommit 7922af8a7595881503d8db4f26aa3b537d8f3668[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Jan 1 18:48:03 2020 +0100

    Add STM32F4 targets

[33mcommit eb365c08e95d41bddb13813104f69b977f29d954[m[33m ([m[1;33mtag: v0.3.0[m[33m)[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Jan 1 14:54:47 2020 +0100

    0.3.0 rease

[33mcommit 711253d7bdb4f5108cd6081c0a92c7b7d714ec98[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 14:32:25 2019 +0100

    Add c3 ad

[33mcommit 245a53346941d6dcfb582fd42ab201bc5ab41444[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Dec 31 17:03:25 2019 +0100

    Fix the automatic chip detection (#106)
    
    * Fix autodetection for nRF chips
    
    * Improve error message when chip autodetection fails

[33mcommit e696b1278440b87df0b5c20822dd858923ee1eb8[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Tue Dec 31 11:37:05 2019 +0100

    Better explain usage of cargo-flash

[33mcommit bffbea5bd08956d30bbfbc22bab519a5ef6b2478[m
Merge: c3c815b 94732d9
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Dec 31 11:02:24 2019 +0100

    Merge pull request #95 from probe-rs/use-block-transfer
    
    Use block transfer

[33mcommit 94732d95b185cc940d34c70efb23abd05095e214[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Tue Dec 31 10:44:08 2019 +0100

    Use log::debug! instead of debug!

[33mcommit 6343611fa9b715653d36d06a5e52f8dcb2aff827[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Tue Dec 31 10:27:31 2019 +0100

    Remove commented out code

[33mcommit 2536748a9f42e0c1a8ad40d064b5a15c448b9481[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Mon Dec 30 13:04:09 2019 +0100

    Change naming to be more consistent

[33mcommit be126709a45b8e1d9e164e7c8f6f135b315e3fdd[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sat Dec 28 23:45:16 2019 +0100

    Use automatic target selection in example

[33mcommit a341cf5c4caaef0457c2ed930782c1e333ca0fc0[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sat Dec 28 23:13:29 2019 +0100

    Fix example to work with newest structopt version

[33mcommit e9d5f4c75d6625da5cbc94a7f5eabe313530de6b[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sat Dec 28 23:08:50 2019 +0100

    Use block transfer for large reads

[33mcommit 1fbd15a6b3ff5957fc446ae2e0d41156d1a1fdbe[m
Author: Dominik <dominik.boehi@gmail.com>
Date:   Sat Dec 28 22:18:15 2019 +0100

    Fix tests for block write

[33mcommit c3386ac9b01304f4dff53b2825f5c0eb918f10a3[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Dec 28 21:36:46 2019 +0100

    Use block transfer for large memory writes

[33mcommit c3c815b14ab5de2b11e7600cc7566df4d848fd25[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Mon Dec 30 03:47:34 2019 +0100

    Update dependencies (#99)
    
    * Update probe-rs dependencies
    
    * Replace objekt with dyn-clone as the crate got renamed.
    
    * Update probe-rs-cli dependencies

[33mcommit 30e560c4ffd207d6b6668eb59d8d54b71121abfb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 16:14:46 2019 +0100

    Make flashing interface cleaner

[33mcommit 0d63e54090994ac4c3d0e5f70d06844d9b993d84[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 16:06:29 2019 +0100

    Run fmt and clippy

[33mcommit 6615b90e7686d14be613f57366e186dd9b5236ad[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 16:05:12 2019 +0100

    Beatify bar. Display progress in bytes. Fix alignment

[33mcommit 71987cdbd9134a258b92d3b560d8ed9b79532f6b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 15:39:22 2019 +0100

    Display proper times for progress bars. Only start second progress bar after the first one. Fix the spinner

[33mcommit ef52320a0bc83c609804feaaecfc2499effca08a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 11:34:51 2019 +0100

    Run rustfmt

[33mcommit ad094637e1b89ab93bf920ea6877ec6e3bfa67b1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Dec 28 11:17:04 2019 +0100

    progress now gets reported via a callback. this is way cleaner and easier to use. only the spinner does not update nicely yet.

[33mcommit e509f4249721bfe8bfd730eb830be3d2f5aa8f1f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Dec 25 01:51:54 2019 +0100

    Run rustfmt and clippy

[33mcommit d1643d4e4cab68d6e421f423f6071ae9157433c9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Dec 25 01:48:08 2019 +0100

    Progress bars work

[33mcommit b66da77d278396195b3b154b9ff177e7d94264da[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Dec 24 14:18:29 2019 +0100

    WIP; still no PB printing

[33mcommit 71488f789c33ba1dd80605da32dd6c5664246f23[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Dec 24 14:13:57 2019 +0100

    Add progress reporting; Does not print progress yet

[33mcommit 21540e28b5dc67b139a10196642fb8eabe449619[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Fri Dec 27 21:38:22 2019 +0100

    Add cargo flash example to README

[33mcommit 520bf94b203b5c477cd4a4fb767acf1fc461ef3b[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Dec 19 10:35:12 2019 +0100

    Update README.md

[33mcommit e9e6fcc09ace00edc17c6dc810ff1a1812317ddf[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Dec 19 10:34:58 2019 +0100

    Update CHANGELOG.md

[33mcommit ef6c760508db1d9efaa43a0b2014f78476f73967[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Dec 19 10:28:03 2019 +0100

    Update README.md

[33mcommit d10c66108fbdacfe2fe292cf087761a2b48227df[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Dec 19 10:27:18 2019 +0100

    Update README.md

[33mcommit 6f58c5dd3190358bd17e671b1aef47e0e325402b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Dec 6 23:03:31 2019 +0100

    Move memory module to coresight

[33mcommit 0f5a32e08ca5f387c14f97eaf2d7b40d965612b2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Dec 6 22:56:42 2019 +0100

    Simplyfy romtable module

[33mcommit 8fc177e783da8358347702f5b5eef31f30a4241f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Dec 6 22:54:39 2019 +0100

    Move the debug_probe module into probe

[33mcommit 4c574e47561d5f8c8794c37f39a49667c7482cd6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Dec 6 22:39:49 2019 +0100

    Move the WireProtocol struct

[33mcommit c200806dcf82dc043f769ff144fc3a8fb2fac3f3[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Dec 6 22:36:25 2019 +0100

    Move flash module from probe/ to /

[33mcommit 4dda1fc14e5e1ba633a28503b540857875cc24df[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Fri Dec 6 22:31:23 2019 +0100

    First draft of the config structure for cmsis pack based flashing configuration (#86)
    
    * First draft of the config structure for cmsis pack based flashing configuration
    
    * Remove repositories module as we don't need it for now
    
    * Add range extension function testing
    
    * Updated some types
    
    * Add config parsing
    
    * Fix target selection
    
    * Change everything to the new config structure.
    
    * Add fixed algorithm. It is slow as hell now (example blinky takes 68s)
    
    * Add extracted LPC845
    
    * Clean up flash builder module
    
    * Clean up download.rs & add hex flashing
    
    * Removal of old types that were moved to the config module
    
    * Add LPC55 series
    
    * Add STM32F103 targets
    
    * Combine all chip configs into one family config.
    
    * Clean up code, comment, remove pyocd artifacts
    
    * Improve logging
    
    * Add some docs
    
    * Clean up some real ugly code
    
    * Update cargo-flash/README.md
    
    * Add the m33 to the get_core() method

[33mcommit 885fd1f996090fd87c4007290c80425bce65112a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Nov 25 22:13:08 2019 +0100

    Add initial support for Cortex-M33
    
    Most of the code is based on the M0, with some small adaptions
    to the register definitions.

[33mcommit 4c11534c0a0c2f1f6a5beff3cc9f28b38fe1493d[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Nov 18 23:13:14 2019 +0100

    Cleanup core trait

[33mcommit 2cbbba11eafbac84bd27dd9b68e509e9ed74ac93[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Nov 14 16:24:24 2019 +0100

    Replace the annoying debug calls with trace calls inside the daplink transmitter

[33mcommit edceb0fde9d957b3702b8bdcd848fc20b288a4a0[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Wed Nov 13 17:59:27 2019 -0300

    better unlock process

[33mcommit 5e216ec7fab81c9cfea343d36afc20738c455aab[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Wed Nov 13 03:32:15 2019 -0300

    clippy

[33mcommit 220d313db5390a750552fd4484da4321c167c7ca[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Wed Nov 13 03:18:01 2019 -0300

    setup more tries for nrf unlocking

[33mcommit 2daac6ff4f183b3bb77a32bc14af1aa8983c9da2[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Sun Nov 10 01:19:14 2019 -0300

    only dap worked after real lock

[33mcommit 61f0f8b73b505c882a8a44970b186cf8d37bf84b[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Sat Nov 9 23:57:35 2019 -0300

    nrf unlocking

[33mcommit bacd4ea51e08f357689dc15e84a4b74198db298d[m
Author: thalesfragoso <thales.fragosoz@gmail.com>
Date:   Wed Nov 13 00:31:25 2019 -0300

    fix nrf52832 part number

[33mcommit d937d4f99fe93497aebe0770f3e51489987d205e[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Tue Nov 12 17:57:55 2019 +0100

    log core register along real and expected contents

[33mcommit a15a3a92079e5b08eedd924117579e12c1b208a7[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Nov 12 15:31:23 2019 +0100

    Fix @jonas-schievink s points and run clippy

[33mcommit 1023c00158e048e1aa095d4781f10ad2dd254143[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Nov 12 14:59:29 2019 +0100

    Run cargo fmt

[33mcommit 2d25fa1242eb7e702c716f6075c0cc0c5846f4e6[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Nov 12 14:50:11 2019 +0100

    Fix flash builder :)

[33mcommit 4a0c431e5506200ae4d7bf8794f694ab364f5f32[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Tue Nov 12 16:34:46 2019 +0100

    Remove strange and lonely curly brace in comment (#81)
    
    A lonely curly brace is something sad :'(

[33mcommit 5e1f4eb4f3e4e37187f0c1877d922e7f50221594[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Tue Nov 12 16:01:54 2019 +0100

    Log attaching and check_status of STLink (#80)
    
    Helpful when getting an "UnknownError".
    
    Co-Authored-By: Jonas Schievink <jonasschievink@gmail.com>

[33mcommit 833fb3762fd30db3b83e11968b35657256f00159[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Tue Nov 12 13:17:33 2019 +0100

    Pad error message with 0s, not spaces (#78)

[33mcommit 465a6bca0644aeb5a12874c0e1fbf157d66f4c08[m
Merge: ba809c0 a0ba4dd
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Tue Nov 12 12:46:35 2019 +0100

    Merge pull request #77 from jonas-schievink/downgrade-error-log
    
    Downgrade CIDR preamble mismatch error to warning

[33mcommit a0ba4ddde4e432c8dd79b7fc4758e4feaf95ea9d[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Tue Nov 12 12:19:25 2019 +0100

    Remove unused import

[33mcommit 4b9027033b3c5bccfe6da6a520ef2c8a11e136cc[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Tue Nov 12 12:07:06 2019 +0100

    Downgrade CIDR preamble mismatch error to warning
    
    Also improves the warning to include more information

[33mcommit ba809c0713204c6b2f05f26aa9cba008c097c304[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Tue Nov 12 12:02:56 2019 +0100

    Add pyOCD and stlink to resources (#74)

[33mcommit 08a7e8e70e98e146949ef138721cde195e2eb1e9[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Tue Nov 12 00:30:36 2019 +0100

    Fix links in the CHANGELOG (#72)
    
    Because I'm dumb and didn't test them ;)

[33mcommit 738fd029b96a4fed4a87bd657e8bff26f6c318a6[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Tue Nov 12 00:18:08 2019 +0100

    Add CHANGELOG template (#71)

[33mcommit 6e5aae719079e369af7da4233c84558bc0c8ba8c[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Mon Nov 11 17:03:25 2019 +0100

    Fix read/write_block32 for more than 256 words
    
    From the specs ADI v5 C2.2.2:
    > Automatic address increment is only guaranteed to operate on the 10
    > least significant bits of the address that is held in the TAR.

[33mcommit ec0181562ec7bffe19201b77f2a80ee2eea42457[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Mon Nov 11 22:28:06 2019 +0100

    cargo-flash: Add log messages support (#69)

[33mcommit e661c4e27f86269ccfcccf86e68ef79313e96631[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Mon Nov 11 16:38:50 2019 +0100

    Use `debug` instead of `println` for the IDR

[33mcommit 6102584694428b5aa78ad4758ada2b94cd8ddb2d[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Mon Nov 11 16:37:36 2019 +0100

    Make read_from_rom_table return a result

[33mcommit 73ca00a299b3c38f6b2cfed0b0149b4cdc309231[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Mon Nov 11 15:23:23 2019 +0100

    cargo-flash: improve error handling and printing

[33mcommit f54e011ba6ec5833c6cce8d6455f52e3cdd0f775[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Mon Nov 11 15:23:00 2019 +0100

    Remove unnecessary download error

[33mcommit ae038f8275cb76209268672fefb6d7d80d074382[m
Author: Matt Vertescher <mvertescher@gmail.com>
Date:   Mon Nov 11 15:55:51 2019 +0100

    Fix cli run command in the readme (#67)

[33mcommit dafb431e5b71c9dd5a5956c522fb40462ba39891[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Sun Nov 10 18:05:27 2019 +0100

    Bound `APAccess::Error` by `std::error::Error`

[33mcommit 489a959988885d812fe6d753a570e0d4584087ad[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Sun Nov 10 15:34:46 2019 +0100

    Forward feature arguments to `cargo build`

[33mcommit 64cc5e9ab2649a58e078db1cf5e88732c64a4745[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Sun Nov 10 16:33:23 2019 +0100

    Only remove `flash` argument when it's present

[33mcommit ed78e2bb4c10a3284915731ef8b8253f4e6a9eaa[m
Author: Jonas Schievink <jonasschievink@gmail.com>
Date:   Sun Nov 10 16:38:39 2019 +0100

    Run CI on all Pull Requests

[33mcommit 43971475f1310e6da4dcf139971d7b9e27cb26a6[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sun Nov 10 01:31:59 2019 +0100

    For ST-Link v3, read the version from the correct byte and then ignore it
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 9bf978a5ff271531da7256a468efc844eb3c86a9[m
Author: Raphael Nestler <raphael.nestler@gmail.com>
Date:   Sun Nov 10 00:40:56 2019 +0100

    cargo-flash: Reset target after flashing

[33mcommit 6a61bb83a3aa8f889a64b3a8b3b6bb3b396f016d[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Nov 4 23:13:54 2019 +0100

    Update RESOURCES.md

[33mcommit de6ec25ea6cc60e944c9971211fcb3c6396f36be[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Nov 4 23:13:40 2019 +0100

    Update RESOURCES.md

[33mcommit e1119e4b282e15868ff34a1abd890b7a7f106776[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Thu Oct 31 01:22:02 2019 +0100

    Update issue templates

[33mcommit 1c67a96090252e9dde202c6515fa4c8d0eebb396[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Oct 29 14:04:34 2019 +0100

    Create LICENSE-APACHE

[33mcommit d40547cf155c31590560920531232a0328d5655b[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue Oct 29 14:03:21 2019 +0100

    Create LICENSE-MIT

[33mcommit d80600d80497fdcf54605b39620e55e3704d0d0f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 28 18:38:10 2019 +0100

    Fix signal handling in cargo-flash to compile under Windows

[33mcommit 374e88b7f6951822bef5f311f0d559d999268cbd[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Oct 24 23:39:24 2019 +0200

    Use platform indpendent paths to build probe-rs-targets

[33mcommit 1877849ac851dc1894d95b88710f20260719585c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Oct 24 23:03:28 2019 +0200

    Add Windows build

[33mcommit a02917e08654b600031af4027a16ef8c5a46b60e[m[33m ([m[1;33mtag: v0.2.0[m[33m)[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 23 23:29:15 2019 +0200

    Make crates publish ready

[33mcommit 54d4e0aadae07a46668be88c66b6befd59909d9c[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 23 23:27:57 2019 +0200

    Rename ocd to probe-rs (#50)
    
    * Rename ocd to probe-rs

[33mcommit 6ea0b4e4c119b33ac45cb4b736475604b28219da[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 23 21:29:01 2019 +0200

    Better CLI error handling, auto-select probe and target (fix #2)
    
    Improve CLI error handling by removing some unwraps,
    so that a proper error message will be reported.
    
    Also, automatically select the probe if only a single probe
    is attached to the system, and try to automatically select the
    target.

[33mcommit 954490e787e0fc9e74b204fe1c6461b3c8a94e93[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 23 21:11:24 2019 +0200

    Update README.md
    
    Fix broken bad looking badges.

[33mcommit cd8728e2d9fe39f2465a30c7a8a1121398de660c[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Oct 20 01:34:47 2019 +0200

    Cargo flash fixes (#45)
    
    * Display proper error message when no probe was found
    
    * Do not continue to flash the build artifact if the build failed
    
    * Realize @jschievinks feedback
    
    * Sanitize cargo flash input properly

[33mcommit cb6c74235f1cc4bd3342f6888bddc06138285bfb[m
Author: Erik Svensson <erik.public@gmail.com>
Date:   Sun Oct 20 01:15:16 2019 +0200

    Reset and halt (#47)
    
    * Changed part identifier for the nRF52840 target
    
    Tested on nRF52840-MSK with cargo-flash.
    
    * Adding reset_and_halt method for Core

[33mcommit cc68433c02f3f67888dae360642f506f54351b29[m
Author: Erik Svensson <erik.public@gmail.com>
Date:   Sat Oct 19 00:20:17 2019 +0200

    Changed part identifier for the nRF52480 target
    
    Tested on nRF52840-MSK with cargo-flash.

[33mcommit 541bfa46de4bd45f300e8a918461fab794661e05[m
Merge: 9750bf9 b4d774c
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 17 11:44:55 2019 +0200

    Merge pull request #39 from probe-rs/readme
    
    Update cargo-flash readme

[33mcommit b4d774c0658652e0309ed6f1659fd68fe37da4b1[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Oct 17 11:34:05 2019 +0200

    Implement @therealprof's change

[33mcommit 22ce30a2c1b99d695ed9f0c3d2a508af906601a6[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Oct 17 03:09:05 2019 +0200

    Update cargo-flash readme

[33mcommit 9750bf9631f7ae0b528eb720046b755f929f7bd5[m
Merge: a3403e2 1cd43c7
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 17 09:15:24 2019 +0200

    Merge pull request #38 from probe-rs/rustfmt
    
    Run rustfmt

[33mcommit a3403e23e2f6d33866b4c0a0e3fe1d4dc7c9c6d5[m
Merge: 88adcf6 6d4db9c
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 17 09:12:09 2019 +0200

    Merge pull request #37 from probe-rs/clippy
    
    Fix remaining clippy lints

[33mcommit 1cd43c76317c13a50f9893de501b87159e42df47[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Oct 17 02:52:25 2019 +0200

    Run rustfmt

[33mcommit 6d4db9cd0799dbf3813c4713de831e97a38193ed[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Oct 17 02:26:23 2019 +0200

    Fix remaining clippy lints

[33mcommit 88adcf6e94e52fcebcd92df474be5390f0ca0999[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 17 00:07:27 2019 +0200

    More clippy lint fixing
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit e42b2cf33387951986adae66f2edfab1b5f2f3b4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 23:20:34 2019 +0200

    Incorporate @jschievinks suggestion

[33mcommit 8baf3979fc2c01526ab2769ba918bb42a77ea873[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 23:17:15 2019 +0200

    Add attribution notice

[33mcommit 40da7ae53f23ae4c3600b5890bc22d642a5f6e92[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 23:05:33 2019 +0200

    Fix badges in README

[33mcommit 237242cfd4877a7ea7b471761c320e802aefe668[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 22:59:37 2019 +0200

    Cleanup README

[33mcommit 76f5572c4b0212623c89edc19853008adbd38a3b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 22:10:42 2019 +0200

    Fix warnings

[33mcommit 50886f41721112d8856b7faac7384d5be34edc9c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 22:04:39 2019 +0200

    Added STM32F429, nRF52832, nRF52840 targets. Only flashing is implemented. Only STM32F429 is tested.

[33mcommit 7476748830c1da3c8384a3ca70172218643d0344[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 21:01:32 2019 +0200

    Add proper Aircr support

[33mcommit 3a6d56a38d2043f6d0fdc66fe22e741fd400d87b[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 16 20:28:18 2019 +0200

    Update ocd/src/collection/cores/m4.rs
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit dae77ad8326cc92d8880d975031906f963a52f96[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 16 20:27:48 2019 +0200

    Update RESOURCES.md
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit 034f5ce96b9e384c0090ed0db7d80cc6449fe3ef[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 16 20:27:41 2019 +0200

    Update RESOURCES.md
    
    Co-Authored-By: Dominik Boehi <dominik.boehi@gmail.com>

[33mcommit 22d89250464fe760d84819d0e40f0419d3b36c54[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 02:21:18 2019 +0200

    Add manual links

[33mcommit db55e4f263091e1183bc0d44b33774a493b481a5[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 02:17:20 2019 +0200

    Add M4; For now this is basically a copy of the M0 as the DHCSR and DHRDR seem to be the same

[33mcommit 61f97ae44cb515f648dcc3cc30c64e43fbefde08[m
Merge: 2d4c261 21f90eb
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 16 22:49:19 2019 +0200

    Merge pull request #34 from probe-rs/cleanup-cli-crate
    
    Cleanup clippy lints and format cli crate

[33mcommit 21f90eb6bd36cca172a64216a6dc72964c4f36f0[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 16 22:46:45 2019 +0200

    Cleanup dependencies of cli crate

[33mcommit 6ffba7bc79e6c64aee17858db2a3f815e223ac05[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 16 19:39:35 2019 +0200

    Cleanup clippy lints and format cli crate

[33mcommit 2d4c261c86d6e47da2b320b33780f8c4088be852[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 16 10:18:23 2019 +0200

    Only run on push, avoid running everything twice for pull requests

[33mcommit f39f8577039b221b11ef206b2146ea840f54acaf[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 14 18:32:10 2019 +0200

    Readd workflow file

[33mcommit f103b22e6b0a5b5575b968c45e33c5b8aedfee0c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 14 18:28:07 2019 +0200

    Don't ignore errors for clippy and rustfmt, add annotations

[33mcommit d1c33bd48c1a3d64956f33e5873be6306a893fe2[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 14 18:18:31 2019 +0200

    Install libusb for Linux builds

[33mcommit b01efcad9c1afe82e73a0af523ba6b3dcac59092[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 14 18:11:52 2019 +0200

    Readd GH actions

[33mcommit b6b9b61360712637580bed5506ba356993c2a148[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Wed Oct 16 10:31:35 2019 +0200

    Fix all cargo build/check warnings
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit a26854c1dad9261bbc514df10d7a2680478f3f5e[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Mon Oct 14 13:25:17 2019 +0200

    A couple of clippy lint fixes
    
    Courtesy of `cargo +nightly fix -Z unstable-options --clippy`
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit a2a73edacb290bd2d3c707e0c5b3ad0a2ba3704d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 16 02:08:21 2019 +0200

    Adher to @tiwaluns hints

[33mcommit ed1bfe8364c87b10dc8ae1f2577fcf40e4d14f15[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 15 23:21:56 2019 +0200

    Implement @tiwaluns proposals

[33mcommit 5e2e1fb8b8d2b2dc356f6e7641fbc7ca63594651[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 15 02:55:06 2019 +0200

    Automatic target selection works

[33mcommit 3ba9acd1871fed8d6d33a883f92626ad7a9064be[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 15 01:37:00 2019 +0200

    Loading local config files (/home/yatekii/.config/probe-rs/targets) as well as direct files (-c argument in cargo-flash) works

[33mcommit ad092394fe9660e26cccb2596d2598e0825a8a6f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 15 00:59:25 2019 +0200

    Fix capitalization of flash algorithm path during selection

[33mcommit 2ba5932df9a6d376515e87de88227c22e4d39176[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 15 00:49:12 2019 +0200

    Select flash algorithm on a top level and pass it to functions requiring it instead of having it reside on the target object

[33mcommit f2c5072f661e6fc1907541d7eb7674015f067a71[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Oct 13 16:53:15 2019 +0200

    Prepare target definitions for target autodetection

[33mcommit d6aea725e1ed5cfc665e1a0035fce85ca689d4c6[m
Merge: 30bf1c6 e030aa2
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Oct 15 23:05:28 2019 +0200

    Merge pull request #15 from probe-rs/cleanup-cli
    
    General CLI cleanup

[33mcommit e030aa2199c2f90d95acb9e9af2d13ab55d764b9[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon Oct 14 23:38:27 2019 +0200

    Handle common options in single place

[33mcommit 1e9b8f76b8be03197351eaafb96406d6f49e8e52[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Oct 8 20:58:41 2019 +0200

    Move interactive debugger commands to  separate module

[33mcommit 23f75edef8e3dc223689ce3c604ef2a01ca407aa[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Oct 3 22:47:23 2019 +0200

    Remove old erase command, add help for debug cli, and better debug cli error handling in general

[33mcommit 30bf1c6dc65eb0d6d97f0b288ebed02bc65bd478[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Mon Oct 14 02:38:26 2019 +0200

    Introduce a new function valid_access_ports
    
    This function returns a Vec of all valid access ports found at the
    current target so users don't have to know which ones could exist and
    scan them manually.
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 75bd9c43fcf5371684e27c3572cc976665edd36a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Oct 14 22:10:05 2019 +0200

    Honor entry_present flag

[33mcommit e3349c295dba432edc5a44636461c1e9c22de71a[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Fri Oct 11 23:16:22 2019 +0200

    Simplify RomTable handling so we get some proper data output
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit c499a0d9d58fc3d15a4ed418bd96c0d6bba5d118[m
Merge: c67bfd6 22ec620
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Fri Oct 11 09:13:53 2019 +0200

    Merge pull request #25 from probe-rs/fix-ahb-access
    
    Set necessary bits for AHB Bus access

[33mcommit 22ec6206cbc144633ad26ca330c138fe0e214db1[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Oct 11 08:37:26 2019 +0200

    Ensure the same CSW register is used for memory access everywhere

[33mcommit 8b5c4909006ee7c12161578d984f24c44958d938[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu Oct 10 23:30:23 2019 +0200

    Set necessary bits for AHB Bus access

[33mcommit c67bfd6d3b5479a0d3f6ddd64e79267a3afa1a59[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 10 07:19:28 2019 +0200

    Actually really fix part number detection
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 788a95f08290ff1b801bc08da32cd0a8785f87f9[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Oct 9 23:55:02 2019 +0200

    Fix PART assembly in ComponentID)

[33mcommit a549e4667d27f4b6164d09a32203bbc40b607d71[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 9 01:22:26 2019 +0200

    Update README.md

[33mcommit 4e934c5029d397b726121b0c4d921568060b1da1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 9 01:11:07 2019 +0200

    Change error printlns to eprintln. Also change exit value from 0 to 1 on error.

[33mcommit 8db7ad368aebe4dd96f64d5a6d3599a7c166d9aa[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 9 01:08:03 2019 +0200

    Selecting a custom chip-description file works for cargo-flash.
    + Added error bubbling for target selection.
    + Fixed many other things.

[33mcommit 67a96e2370ac984c8aebee1c5b8949113d32551c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 8 22:58:52 2019 +0200

    Add new target selection to cli & cargo-flash

[33mcommit 67a0374ca5d6e003e0f84b903bcd255840dcdcdd[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 8 19:46:13 2019 +0200

    add get_target

[33mcommit 3824977d3860cc29510f658ac3e83c87a3fc43bf[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Oct 8 19:29:24 2019 +0200

    Add ocd-targets

[33mcommit e9d3d08815e7e518d5ed64e7e23bd1db3347fb9a[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon Oct 7 01:52:34 2019 +0200

    Add definition for STM32F042

[33mcommit 89900f2916ec00608603a6cde8215ee5289907b8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Oct 7 00:09:34 2019 +0200

    Targets can now defined in a yaml file; Store them inside /home/yatekii/.config/probe-rs/targets. They will be automatically loaded when looking for a target; Fixed all warnings;

[33mcommit f5b3a7b2f379aec1b110f5ed2c08785c9b472ace[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Oct 6 16:57:08 2019 +0200

    Add stub hint

[33mcommit dd926a378982f61009a48de36082c9e6eda2a3a1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Oct 6 16:52:18 2019 +0200

    Add stub hint

[33mcommit 126d0f7196cf9380e599b7bcd276e24dd7f7a1ea[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Oct 6 16:49:25 2019 +0200

    Make target chip selectable

[33mcommit 4ffdc33aa0566b4fe75ffb9833d355bc63ec971b[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Oct 9 02:04:26 2019 +0200

    Delete .travis.yml

[33mcommit 3f9927d1bcd26c8086ff3b9b57ce7b1e6306bcc3[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Oct 6 12:27:26 2019 +0200

    No crates (#19)
    
    * Made everything one big crate instead of small ones
    * Clean code a little
    * Clean up versions

[33mcommit bae62c40258791db30d82d427910e34dbc9eaae4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Oct 6 01:08:35 2019 +0200

    Fix the cargo-flash binary after rebasing

[33mcommit 3488f497bcb18bf5509341135002e2d2cadb3d36[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Oct 3 19:17:27 2019 +0200

    More generic commandline options (debug probe instead of ST-Link)
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 4d0e0a62d10515566cda350781fa502c85a70aa0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Oct 5 02:17:59 2019 +0200

    Implement code review changes

[33mcommit c6f9f377f75b1ea21413495a1875086b1c2907c7[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Oct 5 02:04:16 2019 +0200

    remove old comments from pyocd

[33mcommit dedcc17029af3579c76474d4391d8205649e5c97[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Oct 5 01:07:43 2019 +0200

    Move core into target

[33mcommit 38a0f8fe392e3566b136c4692a04f619bbfc12cb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 23:59:27 2019 +0200

    Remove old method for downloading

[33mcommit 22f9d1c8c77d573b97439a4974250aca662b0ef4[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 2 19:11:12 2019 +0200

    Fix issues with NRF 51 flash writer
    
    The NRF 51 flash writer is moved to the cli crate, and now erases
    all pages up to the end of the flash memory.

[33mcommit b78d5346611a576a97a95310d44b5d6201e5adf0[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 2 19:09:49 2019 +0200

    Add missing write to TAR register in write_block32

[33mcommit f1f5d2a45ea9d46fa035fa9a62514400dfe63ac5[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Wed Oct 2 19:08:29 2019 +0200

    Add reset functionality for Cortex M0

[33mcommit 132d8182f7224c28add462a7754405bd717c890a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Oct 3 01:04:49 2019 +0200

    Update readme

[33mcommit 1e9c020b6f5f64e131324d2c48d8400cbdf125a1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 23:51:51 2019 +0200

    Fix crate links

[33mcommit 28c0cc38935ca923f1038eb458913566c6806b62[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 02:05:40 2019 +0200

    Fix metadata for cargo-flash

[33mcommit 759691e56588609a33a1dcdd60b0b6e7e8f2a35c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:57:55 2019 +0200

    Fix some build errors with the probe -> debug_probe rename

[33mcommit 0f7c69bd4f989caa0059575534f1484543ca1cfe[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:49:43 2019 +0200

    Fix wildcard dep

[33mcommit 07021d49516eae3b3421f194d87eed092b7ce699[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:45:57 2019 +0200

    Change probe crate name to debug-probe and add README for cargo-flash

[33mcommit 96358f9a6a3b2d080d6b5057dae356d1a46ac31d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:25:40 2019 +0200

    Rename memory crate for no name conflicts

[33mcommit 652c1ca5759049f39af05ebf67ea35960c94784d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:25:28 2019 +0200

    Rename memory crate for no name conflicts

[33mcommit a0f7e9fe7e98f116a2af998184cf9d6c85febabb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:20:35 2019 +0200

    Add files that went missing in the last commit

[33mcommit e703dd076c0e93d2eaa4717d406161298e6b6c96[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:18:49 2019 +0200

    Fix README.md links and crate descriptions

[33mcommit b4e88ddddfa648cd64df30a752260ac03c2fa1d8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:13:04 2019 +0200

    Make all subcrates commit ready

[33mcommit e0ac1aaa693ca668f04dce3769befebe3852a3a2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 01:02:41 2019 +0200

    Make the coresight crate publish ready

[33mcommit e4a156a683ca3d14e669dda5f68cf3bb95fd8cbf[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Oct 4 00:52:55 2019 +0200

    cargo-flash works \o/

[33mcommit 350caff8d949a6ef152e5b686b47da20d0e432ca[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Oct 3 00:57:25 2019 +0200

    Add forgotten file

[33mcommit 6d6a78c7dda34a81d0e3ae83544e954af9d2898b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Oct 3 00:55:52 2019 +0200

    Start work on cargo flash

[33mcommit c8144c07d8fb7eb5026e5f5a99c981471471d9b2[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 2 22:16:20 2019 +0200

    Fast flashing works

[33mcommit c53801b8779e44d6ce61efc802e0079842168086[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Oct 2 18:59:38 2019 +0200

    RAM contents match now; CPU registers don't

[33mcommit 618d633a3e6db8c358410078749718ea9e7ee67f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Sep 26 19:17:57 2019 +0200

    More debug output

[33mcommit ba082e33a8983ea01a79662d4fb91efe3fb0a768[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Sep 26 00:10:22 2019 +0200

    Debugging state: data does not get written to RAM

[33mcommit 83058f4daac172ac811b2185bfc22797108f90e1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 22:40:22 2019 +0200

    Add disassembly of flash blob

[33mcommit 662e8ef143d888c55d628ee760b1df0c4b43bfc0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 22:25:24 2019 +0200

    Add lot's of debugging logging

[33mcommit cef73985a5201a4afefb77a2f6eaee9fce1ec613[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 19:20:26 2019 +0200

    Verified proper functioning of ELF parsing/loading and added elf debug output

[33mcommit 69b259bd9e5f2883dc48aaf5bbe53853c622d0e4[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Sep 25 18:43:04 2019 +0200

    Add debug output to ELF reader

[33mcommit f055692512fbb750518fc6e94f3d2eb2fada12ca[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 01:15:20 2019 +0200

    Fixed a bug

[33mcommit a931ebb3a0568a781bc7cade3e75d92e85057017[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 00:53:26 2019 +0200

    Add new download method to cli

[33mcommit 6de9ff8dec5508b4153d479cc714fd6956cab061[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 25 00:37:28 2019 +0200

    Added ELF parsing

[33mcommit 10ce06ee802572fca38cbaed11a921a7021a2f1c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Sep 24 23:05:37 2019 +0200

    Flash loader builds again

[33mcommit 08a32f4d9a82799f65dcaeb07808273521e0556f[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Sep 24 18:17:07 2019 +0200

    WIP

[33mcommit de46acb83571ade1abfb7ffefba6f560b89cb934[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Sep 24 01:37:56 2019 +0200

    DOES NOT BUILD; add flash loader

[33mcommit d9dcd2249feec4e689bbd84518090361564142d0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue Sep 24 00:00:08 2019 +0200

    Removed some warnings and routed most of the errors through

[33mcommit 84137b3983fdf01309954608c8e063396f939fd7[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Sep 23 23:09:21 2019 +0200

    Fixed all borrowchecker issues; FlashBuilder code completely untested

[33mcommit aae49c70073303c54723131877e0660754c16537[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Sep 23 01:53:57 2019 +0200

    DOES NOT COMPILE; FlashBuilder implemented; Fighting the borrowchecker atm; TypeState interface makes troubles

[33mcommit 26c9b07a348ab51a9b22cff3d8fac6aafb2cfbb1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 16:17:52 2019 +0200

    DOES NOT COMPILE; Flash builder close to completion; has lot's of compiler errors still.

[33mcommit 4e4c1675f785aaf7f12a2403406847c226473c99[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 04:00:37 2019 +0200

    Add crc computation

[33mcommit e179e88e6b2a4a902c21ddb313a462a2f92cc71c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 03:43:23 2019 +0200

    Add code for memory region description; add memory description of the nRF51822

[33mcommit 96f05d3d6b3c4c2cb650cc71809be8999f572828[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 01:48:59 2019 +0200

    Split TargetInfo trait from Target trait and rename Target trait to Core

[33mcommit d2090cb7f165d1dc2f19e51261156442f76238c6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 00:59:13 2019 +0200

    Implement the remaining flash algo functions

[33mcommit eeffe6cde1f624812ef6288b11cd56a7cd0a04a3[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 22 00:27:55 2019 +0200

    Work on the flash algo impl. Doesn't do anything useful

[33mcommit 3f9f3361ff7399785c46541b6b77150b3e491336[m
Merge: 2d936b2 b46831d
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Sep 21 21:57:00 2019 +0200

    Merge branch 'm0-debug' into flash-algo

[33mcommit b46831dbb4987eb2bcd88d85ab4f20d8d7f39061[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Sep 20 23:23:51 2019 +0200

    Update README.md

[33mcommit 46465c8f72277ca520d65bb7902eaf9fb58b263d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Sep 20 23:05:51 2019 +0200

    Add basic nested type support to variables

[33mcommit 1f86713af8b73a8ee0592f5583471262d1c29e69[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Sep 20 00:47:29 2019 +0200

    Variable eval progress

[33mcommit 5919c4a28218b38884eac413d2043bc78c76d755[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Sep 20 00:18:01 2019 +0200

    Make variables public

[33mcommit 9dd073cd93b686d0208cef07718f7344b3180533[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Sep 19 23:36:13 2019 +0200

    Variables work in basic form; displaying their address works, values are not supported as that needs lots of work

[33mcommit 04f8801606b6e3605a703631e7948a9e5d4bfb4d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Sep 18 23:32:25 2019 +0200

    Wip variables

[33mcommit 16ea65b5efbbcb1840e5905b907d58e521a87c8f[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Sep 13 21:57:46 2019 +0200

    Create separate debugging lib

[33mcommit b5ba9a7b8abb512e9bf2bbd5366a2063e601d8af[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun Sep 8 21:56:27 2019 +0200

    Bugfix: Ensure last frame is shown in stack trace

[33mcommit 12d18752765e3b357590220357f7904eba305278[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 8 21:30:37 2019 +0200

    Implement an iterator for the stack frames. Two issues: a) does not show the last stack frame (weird?) b) does not show the real values of the registers in the first stack frame but the calculated ones like on the other frames.

[33mcommit 842d4d1b8fe83db03d8a929370193341ee4e47be[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 8 20:41:43 2019 +0200

    Much restructuring

[33mcommit d7b54b96d9efe7ef7e0a7221208cfc126b8eed7a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 8 19:03:42 2019 +0200

    Cleanup work

[33mcommit c92a0e213cedadf69ded08bff8eb0c0560bf0c5d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Sep 8 16:12:50 2019 +0200

    Stack unwinding kinda works \o/

[33mcommit 9a73e1fb11f76cee45879cc05383d6629477deef[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun Sep 8 13:32:30 2019 +0200

    Add binary corresponding to sample/dump.txt

[33mcommit 4f13e093804f9a543f37d8c7d33e75055e5944a4[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun Sep 8 13:24:51 2019 +0200

    WIP version of stack unwinding

[33mcommit bc05ef41719464355e28e78b45c6667afc2e3199[m
Merge: eea4fd4 a0cec7e
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Aug 29 13:59:14 2019 +0200

    Merge branch 'master' into m0-debug

[33mcommit 2d936b267e16c7893d610bbb4e5b2af51359425e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu Aug 29 13:31:14 2019 +0200

    Unknown work on flash algorithms

[33mcommit a0cec7e8033b688c92e5b08cdd3f4e2c7db9f619[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Aug 27 19:38:51 2019 +0200

    Add Windows build on Travis

[33mcommit 13bd24b764c99df16ee00539ffcd7c2e23c90d79[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue Aug 27 19:28:47 2019 +0200

    Use rusb instead of libusb-rs, compiles under Windows with bundled libusb

[33mcommit eea4fd435207d70b2563dd6793c2c1e850c891df[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Aug 24 23:09:03 2019 +0200

    Working HW breakpoints using breakpoint unit

[33mcommit 515b37595f0325c7a5feaf2e31a96ec1b2a4de0e[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Wed Aug 21 22:08:13 2019 +0200

    Update RESOURCES.md

[33mcommit 4ab627265e218b5a6ea29d0fcc43698238542013[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 16:33:46 2019 +0200

    Made the target machinery a trait and not a struct.
    + Implemented a Session object.

[33mcommit b39b58e8c5428d0360844fe44b7a7fee895043b0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 16:09:00 2019 +0200

    Flashing algo now uses the new write_block8().
    + Code formatting cleanup

[33mcommit 5dbe7761022d1e1f7df3b915ffc3b24db8d3fc58[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 15:59:48 2019 +0200

    Wiriting u8 blocks works.

[33mcommit e31c14b98add5d68c10ecee0873dfea951497fef[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 15:09:24 2019 +0200

    Aligned 8bit write works.
    + Unaligned 100% broken.

[33mcommit 8bb2401a82143989ccd661af9bc1d4b97dcc322b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 14:05:01 2019 +0200

    Implemented write8.
    + Fix tests.
    + Added stub for write_block8.
    + Fix write_block32 tests (increment was not implemented in the mock).

[33mcommit 21a67ef92ea7669acd20739c75ca71d0143bd7c7[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Jun 10 13:07:13 2019 +0200

    Made memory reading non-generic.
    + Adapted tests to reflect the changes
    + Adapted all other code to reflect the changes
    + Disabled u16 read tests (very rarely needed and thus not implemented
    yet)

[33mcommit 69ff22c2956493ca3aead67fdb9456905c7d400c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat Jun 8 11:31:03 2019 +0200

    Fix tests for memory AP

[33mcommit 73ccc0f321a9827d6f9af10e60c2309fd9935b98[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri Jun 7 23:31:40 2019 +0200

    Simple disassembly shown after halting core.
    
    Also fixed issues with reading memory when not reading words (32bit).
    
    Co-authored-by: Yatekii <doedelhoch4@gmail.com>

[33mcommit 096a7a13a616b5b91aeebf526003dfaa5a4ea6bb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri Jun 7 20:54:01 2019 +0200

    Possibly fix u8/u16 read.
            + To be tested
            + Removed the sample app for the nrf51 from the workspace until
    we fix the mysterious error

[33mcommit ff86142ba730449f16d92ec1ffab71325958145b[m
Author: Noah Hüsser <nh@technokrat.ch>
Date:   Fri May 31 12:30:30 2019 +0200

    Added a sample application for the nrf51 to test debugging with

[33mcommit 458fe0d908ed706a7bfd6945d6aa96abea0d66a4[m
Merge: 3b6ae6c b4d374e
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 30 21:03:06 2019 +0200

    Merge branch 'm0-debug' of github.com:Yatekii/probe-rs into m0-debug

[33mcommit 3b6ae6ca072d2c017c41180c1a068b713b1a5b85[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 30 20:50:39 2019 +0200

    Add breakpoint functionality
    + Make the write enable for the DHCSR register more obvious.

[33mcommit b4d374e2ebb85a4b99cedfc746916f0951b5585c[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Thu May 30 20:46:51 2019 +0200

    Fix step command

[33mcommit 4bc0a3e275ad5621c86063c1ca5a8f41b998a233[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 30 00:05:39 2019 +0200

    Modularized entire debugging logic
    + Stepping is broken
    + Reading the PC always yields the same result (broken?)

[33mcommit 96d43649979bac6e33bf20f85e0f6e84875f134a[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed May 29 19:28:42 2019 +0200

    Fix broken timeout check

[33mcommit cba8540b44526a70d6ee1efb5ba9db496763b472[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed May 29 19:21:59 2019 +0200

    Modularized even more!

[33mcommit 0651d47e1705f2c269e71776dde6bedb6072bb42[m
Author: Noah Hüsser <nh@technokrat.ch>
Date:   Wed May 29 18:26:11 2019 +0200

    Modularized code

[33mcommit 648e2eb101660365d2db3cb719dd7b6c2a477f08[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed May 29 00:16:58 2019 +0200

    Implemented stepping.
    + Not tested.

[33mcommit 07465e810f1523c75556e99c6ce7a60cf2f89d03[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue May 28 22:41:06 2019 +0200

    Fix compilation issue on beta (see rust-lang/rust#60958)

[33mcommit bfe5221be671905f0dc14ddb4d5037aed3b8ee7a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Tue May 28 22:15:09 2019 +0200

    Read PC when halting processor

[33mcommit 371ba749738de68a2ea5a4054d3cee0f978f610a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue May 21 00:12:13 2019 +0200

    Moar README.md updaterinos.

[33mcommit 2c96f8579050ed29bf2855feb4741b65d5e22f73[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon May 27 21:40:59 2019 +0200

    Update RESOURCES.md
    
    Add reference for Cortex-M0 debug

[33mcommit 8033a061f327e5c7ae08ffbbe34a23ea55ea550a[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Mon May 27 21:29:48 2019 +0200

    Very basic debug cli for Cortex-M0

[33mcommit 4a8c4ceb1baff11faa22e8afa01cc7a879962287[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Wed May 22 00:08:40 2019 +0200

    Cleanup work, remove all warnings

[33mcommit 20ded22a5001ea20af1608f8e809b773bd046ac8[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Tue May 21 18:02:50 2019 +0200

    Working code flash for micro:bit (nrf51)

[33mcommit 2086f77658d75e3cd17d7f946265d1835a94d4bf[m
Merge: 57fd8c1 55a0557
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Tue May 21 00:10:12 2019 +0200

    Merge pull request #4 from Yatekii/daplink
    
    Daplink

[33mcommit 55a055797e52d6e584171a509038404ef17d8093[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue May 21 00:09:02 2019 +0200

    Update README.md.

[33mcommit c07bc1c2cc58a173aba63abb9204044fc86f930e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Tue May 21 00:01:24 2019 +0200

    Fixed some issues with the flashing code.
    + Added an example hex file.
    + Added debug output.
    + Still does not work.

[33mcommit f436b3e54c4af1b14d0f2e5a0067f9e3d8eb0f3d[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon May 20 22:45:02 2019 +0200

    Implemented hex flashing.
    + Not even tested once!

[33mcommit 8b89e17cc44269ca1ed447f089b3916651d8c174[m
Merge: dad8556 f02caf0
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 23:24:20 2019 +0200

    Merge branch 'daplink' of github.com:Yatekii/probe-rs into daplink

[33mcommit dad8556537212e73bf7656f1402f047af6da34c6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 23:23:56 2019 +0200

    Writing and Erasing flash over SWD works!

[33mcommit 92fc90ccf5f60b4fcc170d1d5fa2d96b6c0eea2f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 23:22:49 2019 +0200

    Move jep106 crate to a new repositoryMove jep106 crate to a new
    repository..

[33mcommit f02caf0b6a3afa88fdce637f819c1ca50c0dd95b[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun May 19 21:41:08 2019 +0200

    Add missing files from last commit

[33mcommit 054d360e977b2e06fc6743e894bfb142436de766[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun May 19 20:45:59 2019 +0200

    Add typed interface for Debug Port

[33mcommit f8574d981fbd09f0f02c8ef9c958953f239b6fb7[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sun May 19 14:37:34 2019 +0200

    Make getter functions const fn.

[33mcommit 60f186161690cf090f10bac1a780a00b406e001b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 03:48:45 2019 +0200

    Tried implementing a flat iterator over the coresight component tree.
    + Only returns the root.

[33mcommit 609b309f6e48468a1c8de977104b6a33950b0b36[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 03:13:36 2019 +0200

    Parsing of the component tree works properly
    + Renamed many structs to reflect the reality better

[33mcommit d2c9722f73f764518aed04141b7eb365304866b1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 19 01:49:55 2019 +0200

    Add lots of docs.

[33mcommit 3a742115e503407c384053d94132fc87457a2944[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Sat May 18 23:57:56 2019 +0200

    Rework rom table iteration

[33mcommit 3f15045cc9661b532496a50a4e1cba40e6d67790[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sat May 18 17:17:44 2019 +0200

    RomTable should now have a properly recursive try_parse()

[33mcommit 83ad8ba8951cc7a2783a23a646a83c8af6194437[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 16:03:33 2019 +0200

    Started recursive marcher and data structure for romtables
    + Surely does nothing good
    + Compiles

[33mcommit 531c887c14eeef337beb6be1e126d5d6e755135f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:35:19 2019 +0200

    Fulfill cargo publishes wishes ...

[33mcommit f2e831e72618eb4c63732f9ee7d33866a410e464[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:29:34 2019 +0200

    Remove obsolete file.

[33mcommit 6a573a94e7d302489c3c437825692ddaabe37173[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:28:48 2019 +0200

    Add required license meta tag
    + Add categories

[33mcommit 9234581102feab1bbf3df10e7c49ed6f41da3df1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:18:42 2019 +0200

    Modularize cli code
    + Fix compiler errors due to jep106 changes.

[33mcommit 39517c5782e50b9c3913887264f22a8d4a071d2e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:18:17 2019 +0200

    Add a proper implementation of Debug for JEP106Code.

[33mcommit 9121b6932b4b4d979918c58f3c02f24b85ede433[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:11:02 2019 +0200

    Add meta information for the crate.

[33mcommit 37e86d32295cff9cfd68bda1988f7a671e4c7a8a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 18 00:00:49 2019 +0200

    Polish JEP106 crate for publish.

[33mcommit 0384415fc50c4844a214eaca45332766ce299097[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri May 17 20:48:34 2019 +0200

    WIP: Recursive parsing of ROM tables

[33mcommit 57fd8c12597027dcde251e2f53cceb53ddcac143[m
Author: Dominik Boehi <dominik.boehi@gmail.com>
Date:   Fri May 17 12:49:34 2019 +0200

    Update RESOURCES.md
    
    Add link to coresight architecture specification

[33mcommit baf231567ed24d035dc8983ca2d3e1db4f2eec63[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 17 10:55:16 2019 +0200

    Fix ROMtable readings
    + Reads the JEP106 codes properly now

[33mcommit a37fcb980a8655966fdeee976926a15e385ceb3b[m
Merge: c6ec7b1 cd2ded2
Author: Noah Huesser <admin@yatekii.ch>
Date:   Fri May 17 01:10:57 2019 +0200

    Merge branch 'daplink' of https://github.com/Yatekii/probe-rs into daplink

[33mcommit c6ec7b1928d3860ec2e74067c1f739520b1886e7[m
Author: Noah Huesser <admin@yatekii.ch>
Date:   Fri May 17 01:09:58 2019 +0200

    Fix error

[33mcommit 8dd55336a0b0dec489858b444e3938cea88ad87b[m
Author: Noah Huesser <admin@yatekii.ch>
Date:   Fri May 17 01:05:36 2019 +0200

    Add jep106 crate which can return all manufacturer strings for a given
    ID.

[33mcommit cd2ded2dfc3fd1a2304a44332590391c02267e0f[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Fri May 17 00:50:19 2019 +0200

    WIP: Read rom table entries

[33mcommit fcbe8dbb5a533caeb50e1a7845d3e7971aed6078[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 16 23:07:04 2019 +0200

    Restructured entire component identification code
    + CIDR reading works
    + PIDR reading works

[33mcommit f8f542ae8ce0c30f553b4e80dbda35d1718c01f0[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 16 19:53:58 2019 +0200

    Add a logging facility
    + Slight code cleanup

[33mcommit 9f05fccb5c8ef4882f6665a94770eb6f884e6653[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Thu May 16 00:03:17 2019 +0200

    Use proper port (access port or debug port) when reading / writing registers

[33mcommit 5df55e4558289f5a9caccd3ade12fe6a7a28a693[m
Merge: 998a525 74290d4
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 11 02:42:29 2019 +0200

    Merge branch 'to' into daplink
    + Fixed all warnings
    + Maybe buggy when reading IDR
    + Reading IDCODE works

[33mcommit 74290d4b5c434f487eac8dd8ab54d383f6c87c0d[m
Merge: 903d629 38aeea6
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Fri May 10 23:40:12 2019 +0200

    Merge pull request #3 from Yatekii/master_probe_suggestion
    
    WIP: Use a wrapping struct to handle different probes

[33mcommit 38aeea634f628c6d0e8684bae982b37e52526ab2[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Thu May 9 17:59:10 2019 +0200

    Reintroduce errors for register access, move DAPAccess trait to probe crate

[33mcommit a68b11f709894fed248762fd4c4d532a1cbe0ab2[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Thu May 9 00:43:49 2019 +0200

    Use a probe struct which contains the actual specific probe as a trait object

[33mcommit 903d629157faf4c12dcd14e7a3cb98f5206b91e5[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed May 8 14:55:57 2019 +0200

    Trait Object version works

[33mcommit 998a525d263a52fd144fa5b3c71fde2d944d14a8[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Tue May 7 23:02:18 2019 +0200

    Add remaining memory AP registers and some documentation

[33mcommit 0f005ba85b797525e4eb16ab7d54fee58ea8585f[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Mon May 6 23:26:27 2019 +0200

    Use correct access port to read data

[33mcommit 6eb3fb47fcc23b8070deec520485dbf0bcde0ac4[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Mon May 6 18:54:57 2019 +0200

    Use correct product ID when accessing DAPlink

[33mcommit 4aff915cbbca3427db7706a9a96b744191f13956[m
Merge: 67b8262 ae42559
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Mon May 6 07:08:43 2019 +0200

    Merge remote-tracking branch 'origin/daplink' into daplink

[33mcommit 67b8262460e519a8c1d38fecf4bef9e95f2e29ee[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Mon May 6 07:08:14 2019 +0200

    Added methods to read from AP (not DP), fix address encoding issue in TransferRequest

[33mcommit ae4255911943628cefab24ca198640e6b21576f2[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon May 6 01:20:24 2019 +0200

    Add a unified list function

[33mcommit 48521875c5c3f42d9593bf782cde9fb9b8023e23[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon May 6 00:56:35 2019 +0200

    Unify interfaces to list STLinks as well as DAPLinks and construc them from an info struct

[33mcommit ac8f87a02be5d5a508d20676678ee576aba22ea9[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 21:01:24 2019 +0200

    It works! \o/
    
    Also added the configure SWS command

[33mcommit ec9e08bd7c707117cf480e1a579b065978c8dd66[m
Merge: 9782ad6 44af6e7
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 20:15:07 2019 +0200

    Merge remote-tracking branch 'origin/daplink' into daplink

[33mcommit 9782ad63d157baa8c296ef5106fe5b7afab03d8b[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 20:13:11 2019 +0200

    Add DAP_SWJ_SEQUENCE command

[33mcommit 44af6e78beb16eb8776a44d16776c9dbfcd6df7a[m
Merge: 60f5f51 27af204
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 18:45:10 2019 +0200

    Merge branch 'daplink' of github.com:Yatekii/probe-rs into daplink

[33mcommit 60f5f513f53eae963758d08d5849e9cb76a6e6ab[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 18:44:18 2019 +0200

    Implement clock configure command
    + implemenet transfer configure command

[33mcommit 27af20429b842b0e25432614286137656d16ba8b[m
Merge: 09c97f4 c0ad04d
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 17:48:56 2019 +0200

    Merge remote-tracking branch 'origin/daplink' into daplink

[33mcommit 09c97f4cccecec63a8983f24f4b99a1d0d8195f0[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 17:48:09 2019 +0200

    Fix offset issue when reading status of probe

[33mcommit e256f1a869bec4bb834245e43e9c4033798ddc2e[m
Author: Dominik Böhi <dominik.boehi@gmail.com>
Date:   Sun May 5 17:46:56 2019 +0200

    Add specific errors for register read / write

[33mcommit c0ad04d343c947f49290710deea50d6b7ac4a26e[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 17:17:07 2019 +0200

    Implement target reset
    + does not do anything :/

[33mcommit 025e5b5bb57c2d75e566e5d35cff759ebff6b10e[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 16:05:52 2019 +0200

    Fix mistakes in the transfer command serialization

[33mcommit 53d87ce0a65b0bcafaeb6e22072dfe351d908500[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 15:43:25 2019 +0200

    Fix transfer request serialization

[33mcommit c7a6e79210321ab27215014df4cb62587e1b25b6[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 14:38:09 2019 +0200

    Improve log messages

[33mcommit 3a94e4e630d0293fa81d14097d4bf9ad407d0bfe[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 14:34:19 2019 +0200

    Impl read_block
    + Still panics

[33mcommit 57314cfa73d30ec5e60d8d000a13dd062b9b9812[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 14:12:40 2019 +0200

    Add implementation for DAPLink memory read with ADIv5.2 traits
    + Panics ...

[33mcommit e6099ea99667115df6b16140f581005cb40b9007[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun May 5 13:00:58 2019 +0200

    Implement {read,write}_register
    + Not tested
    + Very possibly has panics because of no/bad boundary checks

[33mcommit 5adc062fab30d6d0feb1e840e194b5f67d1722cb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun May 5 00:23:04 2019 +0200

    Implemented DAPLink for transfer
    + Not tested
    + MemoryAccessPort interface not implemented properly yet

[33mcommit f00b0074f32dc527ecf7ef66fb3412cd7f02e0bc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 4 18:14:52 2019 +0200

    Attach/Detach works

[33mcommit 81805b1c9e3b1bb0ff3e93ae737f4e770a793777[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 4 02:34:15 2019 +0200

    Reading info from the DAPLink works.

[33mcommit b89a94b86777345d04198b4d213ac655ac32297d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat May 4 02:22:14 2019 +0200

    HIDAPI experiments

[33mcommit 2a9106e603c954d49d8e9f160d7bfed011117518[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Fri May 3 01:45:39 2019 +0200

    Implement basic USB interface
    + Does not compile
    + Needs lots of work
    + Maybe we need to use the crate hidapi!

[33mcommit dc53f4ea20d5af1e74799b875ad76be90c3b1ebf[m
Merge: 18fabb1 0bf8853
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Thu May 2 01:46:55 2019 +0200

    Merge branch 'ap-exp'
    + Impl info, connect and disconnect commands
    + Not tested

[33mcommit 0bf88534745fe37a6d8904c30653d3073c054dd6[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed May 1 18:54:36 2019 +0200

    Various cleanup

[33mcommit 18fabb14bd49744df5a366e178f2c6e45be03a4a[m
Author: Noah Huesser <admin@yatekii.ch>
Date:   Wed May 1 18:44:01 2019 +0200

    Start daplink impl. Does NOT compile.

[33mcommit eaa79df692ff34116c162ce14ce264c6304c5716[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sat Mar 9 13:13:34 2019 +0100

    Simply map the error instead of going through a custom trait
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 19670c121c0647117b277d88b966ee207cf8e810[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Thu Mar 7 06:53:04 2019 +0100

    Fixed test case
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 2a6b2208a3cf7be1b3cc85f43df7fdf310408850[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sun Mar 3 19:52:12 2019 +0100

    Replaced associated type for Error by fixed Error type
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 472b5a33b3b308ea2da1bce3677684db2f758de4[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Sun Mar 3 19:45:04 2019 +0100

    Revamp and move the memory interface to reverse dependencies
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 6786a1513b72d87d61fb61098b9cdc6817888d53[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Fri Mar 1 19:16:58 2019 +0100

    Get rid of associated Error type for DebugProbe and use type directly instead
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 0f61b7b492de6012d0264dea806cda637df665f5[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Feb 27 17:09:46 2019 +0100

    Fixed broken type for designer field on IDR

[33mcommit c8a1b94dbec7b6efa2197fb6ff4a7fa9abcd1d74[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Feb 27 16:58:50 2019 +0100

    Fixed ROMtable address.
    + Reads still wrong

[33mcommit 89e54e63785bdb82b3b839b4824c391da1ed7171[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Feb 27 16:49:03 2019 +0100

    Fixed broken merge

[33mcommit 016726b1b0b32db9afe8e60ae3848b5283b930ab[m
Merge: 67d205f 5c7eae6
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Feb 26 19:14:33 2019 +0100

    Merge branch 'ap-exp'

[33mcommit 67d205f61149b54d15a16b5b5f93e55a25f264c3[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Feb 26 19:02:57 2019 +0100

    fruitless tries to read romtable

[33mcommit c3cdff2ed5a00e3a7f438b0d0cb63890c3cf6dd8[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon Feb 25 21:49:13 2019 +0100

    Added the BASE register

[33mcommit 5c7eae6ce9bccd9016834cae69790a628aa083e8[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun Feb 24 22:06:14 2019 +0100

    Fixed compiler error

[33mcommit b0825785be967e6446e87187d68180a87052dc12[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Feb 24 21:45:56 2019 +0100

    ...

[33mcommit 8e9ad2479e6f98c67db1e3358a5376ccf131ad08[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Feb 24 21:45:38 2019 +0100

    Make DebugProbe require MI

[33mcommit f8a8ce673d993520e638bba82007b118aed08bcc[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 24 20:39:28 2019 +0100

    Implemented a memory interface on the STLink

[33mcommit 3563f1d7b3b1a6fba2a4774464312b07974d7d3b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 24 18:08:35 2019 +0100

    AP interface experiments

[33mcommit 8eeea25f007b96a46b2c5c17ce6b1694a5510712[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sun Feb 24 15:43:11 2019 +0100

    Update RESOURCES.md

[33mcommit 7b9699cf7efdd81f67cdee387c007849228b0ea9[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 24 02:34:19 2019 +0100

    Implemented IDR reading
            + Reads weird value ...

[33mcommit 6a3aca6e300ff417179ecaedd79c4f6c7fe76d3c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 23:54:45 2019 +0100

    AP register generation macro works now

[33mcommit 79d09727e49011ad48e1a565d6e1231b1ebfd378[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 23:45:05 2019 +0100

    Macros work but reading doesn't anymore

[33mcommit b46a3b751ecc00123f33acc653556bb4c9db8eb5[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 22:58:00 2019 +0100

    More cleanup

[33mcommit 311f1e7963c22c862264995e7376410d7f42424c[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 22:55:53 2019 +0100

    Fixed some more clippy hints and disabled clippy again because it exhibits weird suggestions ...

[33mcommit 08d352d48b642c00e833ae22baca1b4aa9ea8650[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 19:32:10 2019 +0100

    Added clippy to travis

[33mcommit 81b86c4fb02e5420f92495a27ed833bc2e1e746f[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 19:30:36 2019 +0100

    Fixed all clippy complaints!
            + general cleanup

[33mcommit a0ffafa2a3a5cb2a7ec48ac21b6cefd9ecef3a0d[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 18:22:30 2019 +0100

    Fixed all the known issues

[33mcommit 381d87a4bb87c43eb35abf646ef5d4ebe2aa0b6e[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 17:50:42 2019 +0100

    Fixed reading of values of all sorts

[33mcommit b1f939430e226e8858e0412ec76c3f9310afb786[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 17:17:50 2019 +0100

    Reads are always 8 bits; didn't locate the cause yet.

[33mcommit 39a7dd68d1d19ea5994b4a1d9e0e1aeca2c1f67a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 23 16:14:20 2019 +0100

    Ported all tests for the memory AP to the new typed interface.

[33mcommit ce3a6f999bb9e7bd350427d1f64997efa6d562c7[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sat Feb 23 12:52:23 2019 +0100

    r/w with the new, typed AP interface works.
            + Includes tests for those
            + Block r/w not done yet
            + Needs lots of cleanup

[33mcommit e379a845e5fe76fc1cfcbc4edd49607638f329ca[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sat Feb 23 02:59:41 2019 +0100

    Readme language fix

[33mcommit 38301b6283bab5b07475b7a55134d4552d771c1f[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Sat Feb 23 02:27:30 2019 +0100

    Update RESOURCES.md

[33mcommit 4d944dae5867c843b9d1fb25f2a8b8885b28a40a[m
Merge: 68bfd69 6c97a8a
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sat Feb 23 01:11:47 2019 +0100

    Merge branch 'master' of github.com:Yatekii/probe-rs

[33mcommit 68bfd6973f12e13bc27bd518bf92a1c532f5896c[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sat Feb 23 01:06:04 2019 +0100

    New typed AP interface
    + Still needs a lot of cleanup
    + Still needs a macro for register defs
    + Tests fix still direly needed

[33mcommit 6c97a8a3882c9655f587b217e16e417a2db14d71[m
Author: Daniel Egger <daniel@eggers-club.de>
Date:   Fri Feb 22 20:40:51 2019 +0100

    Turn STLinkError into a generic DebugProbeError
    
    Signed-off-by: Daniel Egger <daniel@eggers-club.de>

[33mcommit 340947fcde51776f2edfd33664b8e071ef5ad79b[m
Author: Daniel Egger <daniel.egger@axiros.com>
Date:   Fri Feb 22 20:25:10 2019 +0100

    Added get_name() function to DebugProbe
    
    Signed-off-by: Daniel Egger <daniel.egger@axiros.com>

[33mcommit 690e4ab7daf3ae0a672d7e288b405da5adcb45d9[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Feb 21 00:52:05 2019 +0100

    Fixed a bug where every action would always panic when using the stlink usb interface.

[33mcommit 2c3dc3e1c9e4cf35f636972b675d06d2c89fa65e[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Feb 21 00:36:57 2019 +0100

    Renamed ST-Link creator function and added some description for it.
    + Some warning cleanup

[33mcommit 887f29ec89c34eb9c9e32657427b2e9b319a3b52[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Thu Feb 21 00:32:47 2019 +0100

    Changed the STLink to contain the USB context.

[33mcommit dc94b30f3051bd33716c395280f0b75a6a850cc4[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Feb 20 02:48:09 2019 +0100

    Updated mocks to use the new AP interface instead of the DAP one.
    + Tests not updated or buildable atm.
    + Not happy with how the API is atm, because any register type can be matched with any value type which really should not be possible.

[33mcommit 4d2fa70aa25de16059538ed50d730712eb2530e8[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Wed Feb 20 00:28:06 2019 +0100

    Added the generic AP traits
    + Does not feature real MEM-AP regs yet
    + Compiles again
    + Needs testing!

[33mcommit 2f2ed2de2aabb34a474ef8a356e315dd3c96127e[m
Merge: 8373d5e 5eee231
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Feb 19 23:32:07 2019 +0100

    Merge branch 'master' of github.com:Yatekii/probe-rs

[33mcommit 8373d5e1f530c312f3d30ce26b1a8788fcaeb627[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Tue Feb 19 23:30:48 2019 +0100

    Work to make AP registers typed;
    DOES NOT COMPILE

[33mcommit 5eee23109d540c66b47738c1553b14e5a78653ac[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Mon Feb 18 22:21:16 2019 +0100

    Added a RESOURCES file where we can track information about specific APIs etc.

[33mcommit 4279ddc86643cdc1d1132e339aee38e51040aec3[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Feb 18 18:11:11 2019 +0100

    Update README.md
    
    Fixed broken links

[33mcommit 1f5558d307a1596d64495cec18a569b1ca07638d[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon Feb 18 18:09:35 2019 +0100

    Added info about the CLI to the README

[33mcommit 165a3a9d4d6570912bfcde4ba30b851cc1fced6e[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Feb 18 13:43:45 2019 +0100

    Update README.md

[33mcommit f7d77f231485841b404bd21eeb323eebd970fa44[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Feb 18 12:33:28 2019 +0100

    Create README.md

[33mcommit 892df1e1c079cbe8a09dc991ffcb22ce0234da9d[m
Author: Yatekii <Yatekii@users.noreply.github.com>
Date:   Mon Feb 18 12:27:19 2019 +0100

    Create CONTRIBUTING.md

[33mcommit 9036580d74067602bb888fa6dd82568efd4d006c[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Mon Feb 18 01:38:02 2019 +0100

    Fixed build errors on stable
    + Removed futile files

[33mcommit 2e95743207a566866611c8a0225d1939dc8606fd[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 23:58:59 2019 +0100

    Tracing values is now easily possible with a python script.
            + Use `cli trace <n> <address> | python3 cli/update_plot.py

[33mcommit 7eaed2f520be038b784ab3671f3803eb3ca25962[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 17:54:52 2019 +0100

    Tracing works.

[33mcommit 6b9caf08a44a2c3cebb89714aa8a78bc24760a10[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 16:04:52 2019 +0100

    Maybe fixed libusb

[33mcommit b87c67b6c3baaddb5d58cfb1f1fa40662f4c96c1[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 16:02:08 2019 +0100

    Add libusb to travis

[33mcommit 293b698ac8b377e4e5d01f84d3995d6736a19970[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 15:52:52 2019 +0100

    Slight cleanup

[33mcommit 3a7f1ca9e43d50fef0418791f91b4aa51e88010a[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 15:50:48 2019 +0100

    Set up travis

[33mcommit 899652ed9993ab8679c027c61db5ecd80762878b[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 15:47:21 2019 +0100

    Fixed a bug where for 1 word reads the block read functions would error.
    + Removed the counter sample

[33mcommit 9ae9a3b7e6867f361fd9f1583d5cd6eb61efb9c5[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun Feb 17 14:28:31 2019 +0100

    Try and debug USB pipe error on the read_bulk call when reading a register

[33mcommit 06798126fc1c62d7fae7938975b27debe682f00b[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Sun Feb 17 02:12:22 2019 +0100

    Tried to make counter example work to test realtime read; does not work yet.

[33mcommit f1477f0c935dec4654823479f16960e9a0e95893[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 17 00:14:03 2019 +0100

    Removed all occurences of ssmarshal

[33mcommit 18ee6b0eee068316dacf7a240036fa66698036e3[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sat Feb 16 23:49:51 2019 +0100

    Gathered everything to the probe-rs repo
    + Added a clean, generic memory r/w interface
            + Added tests
            + Added docs

[33mcommit 4af5b036af9da9f45665021421664851005366cb[m
Author: Noah Huesser <yatekii@yatekii.ch>
Date:   Wed Feb 13 11:53:47 2019 +0100

    Relocated the DAPAccess and DebugProbe traits to respective repositories (coresight-rs and probe-rs)

[33mcommit f534ea784c51079d7db28f63601fbf39476173cb[m
Author: Noah Hüsser <yatekii@yatekii.ch>
Date:   Sun Feb 10 21:13:23 2019 +0100

    Initial commit with a very rough and preliminary proposal of an API.
