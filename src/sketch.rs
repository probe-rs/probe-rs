pub(crate) fn notmain() -> anyhow::Result<i32> {
    // - parse CL arguments
    // - parse ELF -> grouped into `ProcessedElf` struct
    //   -> RAM region
    //   -> location of RTT buffer
    //   -> vector table
    // - extra defmt table from ELF
    // - filter & connect to probe & configure
    // - flash the chip (optionally)
    // - write stack overflow canary in RAM
    // - set breakpoint
    // - start target program
    // - when paused, set RTT in blocking mode
    // - set breakpoint in HardFault handler
    // - resume target program
    // while !signal_received {
    //   - read RTT data
    //   - decode defmt logs from RTT data
    //   - print defmt logs
    //   - if core.is_halted() break
    // }
    // - if signal_received, halt the core
    // - [core is halted at this point]
    // - stack overflow check = check canary in RAM region
    // - print backtrace
    // - reset halt device to put peripherals in known state
    // - print exit reason

    todo!()
}

struct BacktraceInput {
    probe: (),
    // .debug_frame section
    debug_frame: (),
    // used for addr2line in frame symbolication
    elf: (),
}

struct ProcessedElf {
    elf: (), // original ELF (object crate)

    // extracted from `.text` section
    live_functions: (), // name of functinos in program after linking

    // extracted using `defmt` crate
    defmt_table: (),     // map(index: usize) -> defmt frame
    defmt_locations: (), // map(index: usize) -> source code location

    // extracted from `for` loop over symbols
    target_program_uses_heap: (),
    rtt_buffer_address: (),
    address_of_main_function: (),

    // currently extracted via `for` loop over sections
    debug_frame: (),                // gimli one (not bytes)
    vector_table: (),               // processed one (not bytes)
    highest_ram_address_in_use: (), // used for stack canary
}

// impl ProcessedELf {
//     fn symbol_map(&self) -> SymbolMap {
//         self.elf.symbol_map()
//     }
// }

struct DataFromProbeRsRegistry {
    ram_region_that_contains_stack: (),
}

// obtained via probe-rs?
// struct DataFromRunningTarget {}

// fn parse_cl_arguments() -> ClArguments {

// }
