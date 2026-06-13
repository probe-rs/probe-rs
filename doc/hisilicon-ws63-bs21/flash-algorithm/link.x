/*
 * Linker script for the WS63 flash algorithm.
 *
 * Identical to the flash-algorithm crate's memory.x, except PrgCode begins with
 * `KEEP(*(.trampoline))` so a single `ebreak` (defined in main.rs) sits at the
 * algorithm load_address. probe-rs sets `ra = load_address` and relies on a
 * routine's `ret` self-trapping there (CMSIS-Pack convention); the crate itself
 * places functions at `.entry` with no trap, so without this trampoline Init's
 * `ret` falls through into whatever links at offset 0 (EraseSector) and the
 * routine call times out.
 */
SECTIONS {
    . = DEFINED(ALGO_PLACEMENT_START_ADDRESS) ? ALGO_PLACEMENT_START_ADDRESS : 0x0;

    PrgCode : {
        KEEP(*(.trampoline))
        KEEP(*(.entry))
        KEEP(*(.entry.*))

        *(.text)
        *(.text.*)
        *(.text:*)

        *(.rodata)
        *(.rodata.*)

        *(.data)
        *(.data.*)

        *(.sdata)
        *(.sdata.*)

        *(.bss)
        *(.bss.*)

        *(.uninit)
        *(.uninit.*)

        . = ALIGN(4);
    }

    PrgData : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    PrgData : {
        *(.bss .bss.*)
        *(.sbss .sbss.*)

        *(COMMON)
    }

    DeviceData . : {
        KEEP(*(DeviceData))
    }

    /DISCARD/ : {
        *(.ARM.exidx);
        *(.ARM.exidx.*);
        *(.ARM.extab.*);
    }
}
