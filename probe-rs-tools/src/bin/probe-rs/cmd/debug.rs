use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*, Endian,
};
use num_traits::Num;
use parse_int::parse;
use probe_rs::architecture::arm::ap_v1::AccessPortError;
use probe_rs::flashing::FileDownloadError;
use probe_rs::probe::list::Lister;
use probe_rs::probe::DebugProbeError;
use probe_rs::CoreDump;
use probe_rs::CoreDumpError;
use probe_rs::CoreInterface;
use probe_rs::{Core, CoreType, InstructionSet, MemoryInterface, RegisterValue};
use probe_rs_debug::exception_handler_for_core;
use probe_rs_debug::stack_frame::StackFrameInfo;
use probe_rs_debug::{debug_info::DebugInfo, registers::DebugRegisters, stack_frame::StackFrame};
use rustyline::{error::ReadlineError, DefaultEditor};

use crate::{util::common_options::ProbeOptions, CoreOptions};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    shared: CoreOptions,

    #[clap(flatten)]
    common: ProbeOptions,

    #[clap(long, value_parser)]
    /// Binary to debug
    exe: Option<PathBuf>,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister)?;

        let di = self
            .exe
            .as_ref()
            .and_then(|path| DebugInfo::from_file(path).ok());

        let cli = DebugCli::new();

        let core = session.core(self.shared.core)?;

        let mut cli_data = CliData::new(core, di)?;

        let mut rl = DefaultEditor::new()?;

        loop {
            cli_data.print_state()?;

            match rl.readline(">> ") {
                Ok(line) => {
                    let history_entry: &str = line.as_ref();
                    rl.add_history_entry(history_entry)?;
                    let cli_state = cli.handle_line(&line, &mut cli_data)?;

                    if cli_state == CliState::Stop {
                        break;
                    }
                }
                // For end of file and ctrl-c, we just quit
                Err(ReadlineError::Eof | ReadlineError::Interrupted) => return Ok(()),
                Err(actual_error) => {
                    // Show error message and quit
                    println!("Error handling input: {actual_error:?}");
                    break;
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    DebugProbe(#[from] DebugProbeError),
    #[error(transparent)]
    AccessPort(#[from] AccessPortError),
    #[error(transparent)]
    StdIO(#[from] std::io::Error),
    #[error(transparent)]
    FileDownload(#[from] FileDownloadError),
    #[error("Command expected more arguments.")]
    MissingArgument,
    #[error("Failed to parse argument '{argument}'.")]
    ArgumentParseError {
        argument_index: usize,
        argument: String,
        source: anyhow::Error,
    },
    #[error(transparent)]
    ProbeRs(#[from] probe_rs::Error),
    /// Errors related to the handling of core dumps.
    #[error("An error with a CoreDump occured")]
    CoreDump(#[from] CoreDumpError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct DebugCli {
    commands: Vec<Command>,
}

/// Parse the argument at the given index.
fn get_int_argument<T: Num>(args: &[&str], index: usize) -> Result<T, CliError>
where
    <T as Num>::FromStrRadixErr: std::error::Error + Send + Sync + 'static,
{
    let arg_str = args.get(index).ok_or(CliError::MissingArgument)?;

    parse::<T>(arg_str).map_err(|e| CliError::ArgumentParseError {
        argument_index: index,
        argument: arg_str.to_string(),
        source: e.into(),
    })
}

impl DebugCli {
    fn new() -> DebugCli {
        let mut cli = DebugCli {
            commands: Vec::new(),
        };

        cli.add_command(Command {
            name: "step",
            help_text: "Step a single instruction",

            function: |cli_data, _args| {
                let cpu_info = cli_data.core.step()?;
                println!("Core stopped at address 0x{:08x}", cpu_info.pc);

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "halt",
            help_text: "Stop the CPU",

            function: |cli_data, _args| {
                let cpu_info = cli_data.core.halt(Duration::from_millis(100))?;
                println!("Core stopped at address 0x{:08x}", cpu_info.pc);

                let mut code = [0u8; 16 * 2];

                cli_data.core.read(cpu_info.pc, &mut code)?;

                let cs = match cli_data.core.instruction_set()? {
                    InstructionSet::Thumb2 => Capstone::new()
                        .arm()
                        .mode(armArchMode::Thumb)
                        .endian(Endian::Little)
                        .build(),
                    InstructionSet::A32 => {
                        // We need to inspect the CPSR to determine what mode this is opearting in
                        Capstone::new()
                            .arm()
                            .mode(armArchMode::Arm)
                            .endian(Endian::Little)
                            .build()
                    }
                    InstructionSet::A64 => {
                        // We need to inspect the CPSR to determine what mode this is opearting in
                        Capstone::new()
                            .arm64()
                            .mode(aarch64ArchMode::Arm)
                            .endian(Endian::Little)
                            .build()
                    }
                    InstructionSet::RV32 => Capstone::new()
                        .riscv()
                        .mode(riscvArchMode::RiscV32)
                        .endian(Endian::Little)
                        .build(),
                    InstructionSet::RV32C => Capstone::new()
                        .riscv()
                        .mode(riscvArchMode::RiscV32)
                        .endian(Endian::Little)
                        .extra_mode(std::iter::once(
                            capstone::arch::riscv::ArchExtraMode::RiscVC,
                        ))
                        .build(),
                    InstructionSet::Xtensa => Err(capstone::Error::UnsupportedArch),
                }
                .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;

                // Attempt to dissassemble
                match cs.disasm_all(&code, cpu_info.pc) {
                    Ok(instructions) => {
                        for i in instructions.iter() {
                            println!("{i}");
                        }
                    }
                    Err(e) => {
                        println!("Error disassembling instructions: {e}");

                        // Fallback to raw output
                        for (offset, instruction) in code.iter().enumerate() {
                            println!(
                                "{:#010x}: {:010x}",
                                cpu_info.pc + offset as u64,
                                instruction
                            );
                        }
                    }
                };

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "status",
            help_text: "Show current status of CPU",

            function: |cli_data, _args| {
                let status = cli_data.core.status()?;

                println!("Status: {:?}", &status);

                if status.is_halted() {
                    let pc_desc = cli_data.core.program_counter();
                    let pc: u64 = cli_data
                        .core
                        .read_core_reg(pc_desc)?;
                    println!("Core halted at address {:#0width$x}", pc, width = pc_desc.format_hex_width());

                    // determine if the target is handling an interupt

                    if cli_data.core.architecture() == probe_rs::Architecture::Arm {
                        match cli_data.core.core_type() {
                            CoreType::Armv6m | CoreType::Armv7em | CoreType::Armv7m | CoreType::Armv8m | CoreType::Armv7a | CoreType::Armv8a => {
                                // Unwrap is safe here because ARM always defines this register
                                let psr_desc = cli_data.core.registers().psr().unwrap();

                                let xpsr: u32 = cli_data.core.read_core_reg(
                                    psr_desc,
                                )?;

                                println!("XPSR: {:#0width$x}", xpsr, width = psr_desc.format_hex_width());

                                // This is Cortex-M specific interpretation
                                // It's hard to generally model these concepts for any possible CoreType,
                                // but it may be worth considering moving this into the CoreInterface somehow
                                // in the future
                                if cli_data.core.core_type().is_cortex_m() {

                                    let exception_number = xpsr & 0xff;

                                    if exception_number != 0 {
                                        println!("Currently handling exception {exception_number}");

                                        if exception_number == 3 {
                                            println!("Hard Fault!");


                                            let return_address: u64 = cli_data.core.read_core_reg(cli_data.core.return_address())?;

                                            println!("Return address (LR): {return_address:#010x}");

                                            // Get reason for hard fault
                                            let hfsr = cli_data.core.read_word_32(0xE000_ED2C)?;

                                            if hfsr & (1 << 30) == (1 << 30) {
                                                println!("-> configurable priority exception has been escalated to hard fault!");


                                                // read cfsr
                                                let cfsr = cli_data.core.read_word_32(0xE000_ED28)?;

                                                let ufsr = (cfsr >> 16) & 0xffff;
                                                let bfsr = (cfsr >> 8) & 0xff;
                                                let mmfsr = cfsr & 0xff;


                                                if ufsr != 0 {
                                                    println!("\tUsage Fault     - UFSR: {ufsr:#06x}");
                                                }

                                                if bfsr != 0 {
                                                    println!("\tBus Fault       - BFSR: {bfsr:#04x}");

                                                    if bfsr & (1 << 7) == (1 << 7) {
                                                        // Read address from BFAR
                                                        let bfar = cli_data.core.read_word_32(0xE000_ED38)?;
                                                        println!("\t Location       - BFAR: {bfar:#010x}");
                                                    }
                                                }

                                                if mmfsr != 0 {
                                                    println!("\tMemManage Fault - BFSR: {bfsr:04x}");
                                                }

                                            }
                                        }
                                    }
                                }
                            },
                            // Nothing extra to log
                            _ => {},
                        }
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "run",
            help_text: "Resume execution of the CPU",

            function: |cli_data, _args| {
                cli_data.core.run()?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "quit",
            help_text: "Exit the program",

            function: |_cli_data, _args| Ok(CliState::Stop),
        });

        cli.add_command(Command {
            name: "read8",
            help_text: "Read 8bit value from memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                let num_bytes = if args.len() > 1 {
                    get_int_argument(args, 1)?
                } else {
                    1
                };

                let mut buff = vec![0u8; num_bytes];

                if num_bytes > 1 {
                    cli_data.core.read_8(address, &mut buff)?;
                } else {
                    buff[0] = cli_data.core.read_word_8(address)?;
                }

                for (offset, byte) in buff.iter().enumerate() {
                    println!("0x{:08x} = 0x{:02x}", address + (offset) as u64, byte);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "read16",
            help_text: "Read 16bit value from memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                let num_words = if args.len() > 1 {
                    get_int_argument(args, 1)?
                } else {
                    1
                };

                let mut buff = vec![0u16; num_words];

                if num_words > 1 {
                    cli_data.core.read_16(address, &mut buff)?;
                } else {
                    buff[0] = cli_data.core.read_word_16(address)?;
                }

                for (offset, word) in buff.iter().enumerate() {
                    println!("0x{:08x} = 0x{:04x}", address + (offset * 2) as u64, word);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "read32",
            help_text: "Read 32bit value from memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                let num_words = if args.len() > 1 {
                    get_int_argument(args, 1)?
                } else {
                    1
                };

                let mut buff = vec![0u32; num_words];

                if num_words > 1 {
                    cli_data.core.read_32(address, &mut buff)?;
                } else {
                    buff[0] = cli_data.core.read_word_32(address)?;
                }

                for (offset, word) in buff.iter().enumerate() {
                    println!("0x{:08x} = 0x{:08x}", address + (offset * 4) as u64, word);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "read64",
            help_text: "Read 64bit value from memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                let num_words = if args.len() > 1 {
                    get_int_argument(args, 1)?
                } else {
                    1
                };

                let mut buff = vec![0u64; num_words];

                if num_words > 1 {
                    cli_data.core.read_64(address, &mut buff)?;
                } else {
                    buff[0] = cli_data.core.read_word_64(address)?;
                }

                for (offset, word) in buff.iter().enumerate() {
                    println!("0x{:08x} = 0x{:02x}", address + (offset * 8) as u64, word);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write8",
            help_text: "Write a 8bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data: u8 = get_int_argument(args, 1)?;

                cli_data.core.write_word_8(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write16",
            help_text: "Write a 16bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data = get_int_argument(args, 1)?;

                cli_data.core.write_word_16(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write32",
            help_text: "Write a 32bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data = get_int_argument(args, 1)?;

                cli_data.core.write_word_32(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write64",
            help_text: "Write a 64bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data = get_int_argument(args, 1)?;

                cli_data.core.write_word_64(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "break",
            help_text: "Set a breakpoint at a specific address",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                cli_data.core.set_hw_breakpoint(address)?;

                println!("Set new breakpoint at address {address:#08x}");

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "clear_break",
            help_text: "Clear a breakpoint",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;

                cli_data.core.clear_hw_breakpoint(address)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "list_break",
            help_text: "List all set breakpoints",
            function: |cli_data, _| {
                cli_data
                    .core
                    .hw_breakpoints()?
                    .into_iter()
                    .enumerate()
                    .flat_map(|(idx, bpt)| bpt.map(|bpt| (idx, bpt)))
                    .for_each(|(idx, bpt)| println!("Breakpoint {idx} - {bpt:#010X}"));

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "bt",
            help_text: "Show backtrace",

            function: |cli_data, _args| {
                match cli_data.state {
                    DebugState::Halted(ref mut halted_state) => {
                        if let Some(di) = &mut cli_data.debug_info {
                            let initial_registers = DebugRegisters::from_core(&mut cli_data.core);
                            let exception_interface =
                                exception_handler_for_core(cli_data.core.core_type());
                            let instruction_set = cli_data.core.instruction_set().ok();
                            halted_state.stack_frames = di
                                .unwind(
                                    &mut cli_data.core,
                                    initial_registers,
                                    exception_interface.as_ref(),
                                    instruction_set,
                                )
                                .unwrap();

                            halted_state.frame_indices = halted_state
                                .stack_frames
                                .iter()
                                .map(|sf| sf.id.into())
                                .collect();

                            for (i, frame) in halted_state.stack_frames.iter().enumerate() {
                                print!("Frame {}: {} @ {}", i, frame.function_name, frame.pc);

                                if frame.is_inlined {
                                    print!(" inline");
                                }
                                println!();

                                if let Some(location) = &frame.source_location {
                                    print!("       ");

                                    print!("{}", location.path.to_path().display());

                                    if let Some(line) = location.line {
                                        print!(":{line}");

                                        if let Some(col) = location.column {
                                            match col {
                                                probe_rs_debug::ColumnType::LeftEdge => {
                                                    print!(":1")
                                                }
                                                probe_rs_debug::ColumnType::Column(c) => {
                                                    print!(":{c}")
                                                }
                                            }
                                        }
                                    }

                                    println!();
                                }
                            }

                            println!();
                        } else {
                            println!("No debug information present!");
                        }
                    }
                    DebugState::Running => {
                        println!("Core must be halted for this command.");
                    }
                }
                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "regs",
            help_text: "Show CPU register values",

            function: |cli_data, _args| {
                match &cli_data.state {
                    DebugState::Running => println!("Core must be halted for this command."),
                    DebugState::Halted(state) => {
                        if let Some(current_frame) = state.get_current_frame() {
                            let registers = &current_frame.registers;

                            for register in &registers.0 {
                                print!("{:10}: ", register.core_register.name());

                                if let Some(value) = &register.value {
                                    println!("{:#}", value);
                                } else {
                                    println!("{}", "X".repeat(10));
                                }
                            }
                        } else {
                            let register_file = cli_data.core.registers();

                            for register in register_file.core_registers() {
                                let value: RegisterValue = cli_data.core.read_core_reg(register)?;

                                println!("{:10}: {:#}", register.name(), value);
                            }

                            if let Some(psr) = register_file.psr() {
                                let value: RegisterValue = cli_data.core.read_core_reg(psr)?;
                                println!("{:10}: {:#}", psr.name(), value);
                            }
                        }
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "fp_regs",
            help_text: "Show floating point register values",

            function: |cli_data, _args| {
                if !cli_data.core.fpu_support()? {
                    println!("Floating point not supported");
                } else {
                    let register_file = cli_data.core.registers();

                    let registers = register_file.fpu_registers();
                    if let Some(registers) = registers {
                        for register in registers {
                            let value: RegisterValue = cli_data.core.read_core_reg(register)?;

                            // Print out the register every way it can be interpretted.
                            // For example, with a u128:
                            // * Raw value as a u128
                            // * Value cast as a f64
                            // * Value cast as a f32
                            println!("{:10}: {:#}", register.name(), value);

                            if matches!(value, RegisterValue::U128(_) | RegisterValue::U64(_)) {
                                let data: u128 = value.try_into()?;
                                let bytes = (data as u64).to_le_bytes();
                                let fp_data = f64::from_le_bytes(bytes);

                                println!("{:>10}: {:#}", "[as f64]", fp_data);
                            }

                            if matches!(
                                value,
                                RegisterValue::U128(_)
                                    | RegisterValue::U64(_)
                                    | RegisterValue::U32(_)
                            ) {
                                let data: u128 = value.try_into()?;
                                let bytes = (data as u32).to_le_bytes();
                                let fp_data = f32::from_le_bytes(bytes);

                                println!("{:>10}: {:#}", "[as f32]", fp_data);
                            }

                            println!();
                        }
                    } else {
                        println!("Core has no floating point registers");
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "locals",
            help_text: "List local variables",

            function: |cli_data, _args| {
                match cli_data.state {
                    DebugState::Halted(ref mut halted_state) => {
                        let current_frame =
                            if let Some(current_frame) = halted_state.get_current_frame_mut() {
                                current_frame
                            } else {
                                println!("StackFrame not found.");
                                return Ok(CliState::Continue);
                            };

                        let local_variable_cache = if let Some(local_variable_cache) =
                            &mut current_frame.local_variables
                        {
                            local_variable_cache
                        } else {
                            print!("No Local variables available");
                            return Ok(CliState::Continue);
                        };

                        let mut locals = local_variable_cache.root_variable().clone();
                        // By default, the first level children are always are lazy loaded, so we will force a load here.
                        if locals.variable_node_type.is_deferred()
                            && !local_variable_cache.has_children(&locals)
                        {
                            if let Err(error) = cli_data
                                .debug_info
                                .as_ref()
                                .unwrap()
                                .cache_deferred_variables(
                                    local_variable_cache,
                                    &mut cli_data.core,
                                    &mut locals,
                                    StackFrameInfo {
                                        registers: &current_frame.registers,
                                        frame_base: current_frame.frame_base,
                                        canonical_frame_address: current_frame
                                            .canonical_frame_address,
                                    },
                                )
                            {
                                println!("Failed to cache local variables: {error}");
                                return Ok(CliState::Continue);
                            }
                        }
                        let children = local_variable_cache.get_children(locals.variable_key());

                        for child in children {
                            println!(
                                "{}: {} = {}",
                                child.name,
                                child.type_name(),
                                child.to_string(local_variable_cache)
                            );
                        }
                    }
                    DebugState::Running => println!("Core must be halted for this command."),
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "up",
            help_text: "Move up a frame",

            function: |cli_data, _args| {
                match &mut cli_data.state {
                    DebugState::Running => println!("Core must be halted for this command."),
                    DebugState::Halted(halted_state) => {
                        if halted_state.current_frame < halted_state.frame_indices.len() - 1 {
                            halted_state.current_frame += 1;
                        } else {
                            println!(
                                "Already at top-most frame. current frame: {}, indices: {:?}",
                                halted_state.current_frame, halted_state.frame_indices
                            );
                        }
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "down",
            help_text: "Move down a frame",

            function: |cli_data, _args| {
                match &mut cli_data.state {
                    DebugState::Running => println!("Core must be halted for this command."),
                    DebugState::Halted(halted_state) => {
                        if halted_state.current_frame > 0 {
                            halted_state.current_frame -= 1;
                        } else {
                            println!("Already at bottom-most frame.");
                        }
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "reset",

            help_text: "Reset the CPU",

            function: |cli_data, _args| {
                cli_data.core.halt(Duration::from_millis(100))?;
                cli_data.core.reset_and_halt(Duration::from_millis(100))?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "dump",
            help_text: "Dump the core memory & registers",

            function: |cli_data, args| {
                let mut args = args.to_vec();

                // If we get an odd number of arguments, treat all n * 2 args at the start as memory blocks
                // and the last argument as the path tho store the coredump at.
                let location = Path::new(
                    if args.len() % 2 != 0 {
                        args.pop()
                    } else {
                        None
                    }
                    .unwrap_or("./coredump"),
                );

                let ranges = args
                    .chunks(2)
                    .enumerate()
                    .map(|(i, c)| {
                        let start = if let Some(start) = c.first() {
                            parse_int::parse::<u64>(start).map_err(|e| {
                                CliError::ArgumentParseError {
                                    argument_index: i,
                                    argument: start.to_string(),
                                    source: e.into(),
                                }
                            })?
                        } else {
                            unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
                        };

                        let size = if let Some(size) = c.get(1) {
                            parse_int::parse::<u64>(size).map_err(|e| {
                                CliError::ArgumentParseError {
                                    argument_index: i,
                                    argument: size.to_string(),
                                    source: e.into(),
                                }
                            })?
                        } else {
                            unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
                        };

                        Ok::<_, CliError>(start..start + size)
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                println!("Dumping core");

                CoreDump::dump_core(&mut cli_data.core, ranges)?.store(location)?;

                println!("Done.");

                Ok(CliState::Continue)
            },
        });

        cli
    }

    fn add_command(&mut self, command: Command) {
        self.commands.push(command)
    }

    fn handle_line(&self, line: &str, cli_data: &mut CliData) -> Result<CliState, CliError> {
        let mut command_parts = line.split_whitespace();

        match command_parts.next() {
            Some("help") => {
                println!("The following commands are available:");

                for cmd in &self.commands {
                    println!(" - {}", cmd.name);
                }

                Ok(CliState::Continue)
            }
            Some(command) => {
                let cmd = self.commands.iter().find(|c| c.name == command);

                if let Some(cmd) = cmd {
                    let remaining_args: Vec<&str> = command_parts.collect();

                    Self::execute_command(cli_data, cmd, &remaining_args)
                } else {
                    println!("Unknown command '{command}'");
                    println!("Enter 'help' for a list of commands");

                    Ok(CliState::Continue)
                }
            }
            _ => Ok(CliState::Continue),
        }
    }

    fn execute_command(
        cli_data: &mut CliData,
        command: &Command,
        args: &[&str],
    ) -> Result<CliState, CliError> {
        match (command.function)(cli_data, args) {
            Ok(cli_state) => {
                // Resync status from core
                cli_data.update_debug_status_from_core()?;

                Ok(cli_state)
            }
            Err(CliError::MissingArgument) => {
                println!("Error: Missing argument\n\n{}", command.help_text);
                Ok(CliState::Continue)
            }
            Err(CliError::ArgumentParseError {
                argument, source, ..
            }) => {
                println!(
                    "Error parsing argument '{}': {}\n\n{}",
                    argument, source, command.help_text
                );
                Ok(CliState::Continue)
            }
            other => other,
        }
    }
}

pub struct CliData<'p> {
    pub core: Core<'p>,
    pub debug_info: Option<DebugInfo>,

    state: DebugState,
}

impl<'p> CliData<'p> {
    fn new(core: Core<'p>, debug_info: Option<DebugInfo>) -> Result<CliData<'p>, CliError> {
        let mut cli_data = CliData {
            core,
            debug_info,
            state: DebugState::default(),
        };

        cli_data.update_debug_status_from_core()?;

        Ok(cli_data)
    }

    /// Fill out DebugStatus for a given core
    fn update_debug_status_from_core(&mut self) -> Result<(), CliError> {
        let status = self.core.status()?;

        match status {
            probe_rs::CoreStatus::Halted(_) => {
                let registers = DebugRegisters::from_core(&mut self.core);
                let pc: u64 = registers
                    .get_program_counter()
                    .and_then(|reg| reg.value)
                    .unwrap_or_default()
                    .try_into()?;

                // If the core was running before, or the PC changed, we need to update the state
                let core_state_changed = matches!(self.state, DebugState::Running)
                    || matches!(self.state, DebugState::Halted(HaltedState { program_counter, .. }) if program_counter != pc);

                // TODO: We should resolve the stack frames here
                if core_state_changed {
                    self.state = DebugState::Halted(HaltedState {
                        program_counter: pc,
                        current_frame: 0,
                        frame_indices: vec![1],
                        stack_frames: vec![],
                    });
                }
            }
            _other => {
                self.state = DebugState::Running;
            }
        }

        Ok(())
    }

    fn print_state(&mut self) -> Result<(), CliError> {
        match self.state {
            DebugState::Running => println!("Core is running."),
            DebugState::Halted(ref mut halted_state) => {
                if let Some(current_stack_frame) = halted_state.get_current_frame() {
                    let pc = current_stack_frame.pc;

                    println!(
                        "Frame {}: {} () @ {:#}",
                        halted_state.current_frame, current_stack_frame.function_name, pc,
                    );
                }
            }
        }

        Ok(())
    }
}

#[derive(Default)]
enum DebugState {
    #[default]
    Running,
    Halted(HaltedState),
}

struct HaltedState {
    program_counter: u64,
    current_frame: usize,
    frame_indices: Vec<i64>,
    stack_frames: Vec<StackFrame>,
}

impl HaltedState {
    fn get_current_frame(&self) -> Option<&probe_rs_debug::stack_frame::StackFrame> {
        self.stack_frames.get(self.current_frame)
    }

    fn get_current_frame_mut(&mut self) -> Option<&mut probe_rs_debug::stack_frame::StackFrame> {
        self.stack_frames.get_mut(self.current_frame)
    }
}

#[derive(PartialEq)]
pub enum CliState {
    Continue,
    Stop,
}

struct Command {
    pub name: &'static str,
    pub help_text: &'static str,

    pub function: fn(&mut CliData, args: &[&str]) -> Result<CliState, CliError>,
}
