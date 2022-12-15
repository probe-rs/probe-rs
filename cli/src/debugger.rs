use crate::common::CliError;

use anyhow::anyhow;
use capstone::{
    arch::arm::ArchMode as armArchMode, arch::arm64::ArchMode as aarch64ArchMode,
    arch::riscv::ArchMode as riscvArchMode, prelude::*, Capstone, Endian,
};
use num_traits::Num;
use probe_rs::{
    architecture::arm::Dump,
    debug::{
        debug_info::DebugInfo, registers::DebugRegisters, stack_frame::StackFrame, VariableName,
    },
    Core, CoreType, InstructionSet, MemoryInterface, RegisterDescription, RegisterId,
    RegisterValue,
};
use std::fs::File;
use std::{io::prelude::*, time::Duration};

use parse_int::parse;

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
    pub fn new() -> DebugCli {
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
                }
                .map_err(|err| anyhow!("Error creating capstone: {:?}", err))?;

                // Attempt to dissassemble
                match cs.disasm_all(&code, cpu_info.pc) {
                    Ok(instructions) => {
                        for i in instructions.iter() {
                            println!("{}", i);
                        }
                    }
                    Err(e) => {
                        println!("Error disassembling instructions: {}", e);

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
                    let pc_desc = cli_data.core.registers().program_counter();
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
                                        println!("Currently handling exception {}", exception_number);

                                        if exception_number == 3 {
                                            println!("Hard Fault!");


                                            let return_address: u64 = cli_data.core.read_core_reg(cli_data.core.registers().return_address())?;

                                            println!("Return address (LR): {:#010x}", return_address);

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
                                                    println!("\tUsage Fault     - UFSR: {:#06x}", ufsr);
                                                }

                                                if bfsr != 0 {
                                                    println!("\tBus Fault       - BFSR: {:#04x}", bfsr);

                                                    if bfsr & (1 << 7) == (1 << 7) {
                                                        // Read address from BFAR
                                                        let bfar = cli_data.core.read_word_32(0xE000_ED38)?;
                                                        println!("\t Location       - BFAR: {:#010x}", bfar);
                                                    }
                                                }

                                                if mmfsr != 0 {
                                                    println!("\tMemManage Fault - BFSR: {:04x}", bfsr);
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
            name: "read",
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
            name: "read_byte",
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
            name: "read_64",
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
            name: "write",
            help_text: "Write a 32bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data = get_int_argument(args, 1)?;

                cli_data.core.write_word_32(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write_byte",
            help_text: "Write a 8bit value to memory",

            function: |cli_data, args| {
                let address = get_int_argument(args, 0)?;
                let data: u8 = get_int_argument(args, 1)?;

                cli_data.core.write_word_8(address, data)?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "write_64",
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

                println!("Set new breakpoint at address {:#08x}", address);

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
            name: "bt",
            help_text: "Show backtrace",

            function: |cli_data, _args| {
                match cli_data.state {
                    DebugState::Halted(ref mut halted_state) => {
                        let regs = cli_data.core.registers();
                        let program_counter: u64 =
                            cli_data.core.read_core_reg(regs.program_counter())?;

                        if let Some(di) = &mut cli_data.debug_info {
                            halted_state.stack_frames =
                                di.unwind(&mut cli_data.core, program_counter).unwrap();

                            halted_state.frame_indices =
                                halted_state.stack_frames.iter().map(|sf| sf.id).collect();

                            for (i, frame) in halted_state.stack_frames.iter().enumerate() {
                                print!("Frame {}: {} @ {}", i, frame.function_name, frame.pc);

                                if frame.is_inlined {
                                    print!(" inline");
                                }
                                println!();

                                if let Some(location) = &frame.source_location {
                                    if location.directory.is_some() || location.file.is_some() {
                                        print!("       ");

                                        if let Some(dir) = &location.directory {
                                            print!("{}", dir.display());
                                        }

                                        if let Some(file) = &location.file {
                                            print!("/{}", file);

                                            if let Some(line) = location.line {
                                                print!(":{}", line);

                                                if let Some(col) = location.column {
                                                    match col {
                                                        probe_rs::debug::ColumnType::LeftEdge => {
                                                            print!(":1")
                                                        }
                                                        probe_rs::debug::ColumnType::Column(c) => {
                                                            print!(":{}", c)
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        println!();
                                    }
                                }
                            }
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
                let register_file = cli_data.core.registers();

                let psr_iter: Box<dyn Iterator<Item = &RegisterDescription>> =
                    match register_file.psr() {
                        Some(psr) => Box::new(std::iter::once(psr)),
                        None => Box::new(std::iter::empty::<&RegisterDescription>()),
                    };

                let iter = register_file.platform_registers().chain(psr_iter);

                for register in iter {
                    let value: RegisterValue = cli_data.core.read_core_reg(register)?;

                    println!("{:10}: {:#}", register.name(), value);
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

                    if let Some(registers) = register_file.fpu_registers() {
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

                        if let Some(mut locals) = local_variable_cache
                            .get_variable_by_name_and_parent(&VariableName::LocalScopeRoot, None)
                        {
                            // By default, the first level children are always are lazy loaded, so we will force a load here.
                            if locals.variable_node_type.is_deferred()
                                && !local_variable_cache.has_children(&locals)?
                            {
                                if let Err(error) = cli_data
                                    .debug_info
                                    .as_ref()
                                    .unwrap()
                                    .cache_deferred_variables(
                                        local_variable_cache,
                                        &mut cli_data.core,
                                        &mut locals,
                                        &current_frame.registers,
                                        current_frame.frame_base,
                                    )
                                {
                                    println!("Failed to cache local variables: {}", error);
                                    return Ok(CliState::Continue);
                                }
                            }
                            let children =
                                local_variable_cache.get_children(Some(locals.variable_key))?;

                            for child in children {
                                println!(
                                    "{}: {} = {}",
                                    child.name,
                                    child.type_name,
                                    child.get_value(local_variable_cache)
                                );
                            }
                        } else {
                            println!("Local variable cache was not initialized.")
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
                            println!("Already at top-most frame.");
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
            name: "dump",
            help_text: "Store a dump of the current CPU state",

            function: |cli_data, _args| {
                // dump all relevant data, stack and regs for now..
                //
                // stack beginning -> assume beginning to be hardcoded

                let stack_top: u32 = 0x2000_0000 + 0x4000;

                let regs = cli_data.core.registers();

                let stack_bot: u32 = cli_data.core.read_core_reg(regs.stack_pointer())?;
                let pc: u32 = cli_data.core.read_core_reg(regs.program_counter())?;

                let mut stack = vec![0u8; (stack_top - stack_bot) as usize];

                cli_data.core.read(stack_bot.into(), &mut stack[..])?;

                let mut dump = Dump::new(stack_bot, stack);

                for i in 0..12 {
                    dump.regs[i as usize] =
                        cli_data.core.read_core_reg(Into::<RegisterId>::into(i))?;
                }

                dump.regs[13] = stack_bot;
                dump.regs[14] = cli_data.core.read_core_reg(regs.return_address())?;
                dump.regs[15] = pc;

                let serialized = ron::ser::to_string(&dump).expect("Failed to serialize dump");

                let mut dump_file = File::create("dump.txt").expect("Failed to create file");

                dump_file
                    .write_all(serialized.as_bytes())
                    .expect("Failed to write dump file");

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

        cli
    }

    fn add_command(&mut self, command: Command) {
        self.commands.push(command)
    }

    pub fn handle_line(&self, line: &str, cli_data: &mut CliData) -> Result<CliState, CliError> {
        let mut command_parts = line.split_whitespace();

        match command_parts.next() {
            Some(command) if command == "help" => {
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
                    println!("Unknown command '{}'", command);
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
    pub fn new(core: Core<'p>, debug_info: Option<DebugInfo>) -> Result<CliData, CliError> {
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
        // TODO: In halted state we should get the backtrace here.
        let status = self.core.status()?;

        self.state = match status {
            probe_rs::CoreStatus::Halted(_) => {
                let registers = DebugRegisters::from_core(&mut self.core);
                DebugState::Halted(HaltedState {
                    program_counter: registers
                        .get_program_counter()
                        .and_then(|reg| reg.value)
                        .unwrap_or_default()
                        .try_into()?,
                    current_frame: 0,
                    frame_indices: vec![1],
                    stack_frames: vec![],
                })
            }
            _other => DebugState::Running,
        };

        Ok(())
    }

    pub fn print_state(&mut self) -> Result<(), CliError> {
        match self.state {
            DebugState::Running => println!("Core is running."),
            DebugState::Halted(ref mut halted_state) => {
                let pc = halted_state.program_counter;
                if let Some(current_stack_frame) = halted_state.get_current_frame() {
                    println!(
                        "Frame {}: {} () @ {:#010x}",
                        halted_state.current_frame, current_stack_frame.function_name, pc,
                    );
                }
            }
        }

        Ok(())
    }
}

enum DebugState {
    Running,
    Halted(HaltedState),
}

impl std::default::Default for DebugState {
    fn default() -> Self {
        DebugState::Running
    }
}

struct HaltedState {
    program_counter: u64,
    current_frame: usize,
    frame_indices: Vec<i64>,
    stack_frames: Vec<StackFrame>,
}

impl HaltedState {
    fn get_current_frame(&self) -> Option<&probe_rs::debug::stack_frame::StackFrame> {
        self.stack_frames.get(self.current_frame)
    }

    fn get_current_frame_mut(&mut self) -> Option<&mut probe_rs::debug::stack_frame::StackFrame> {
        self.stack_frames.get_mut(self.current_frame)
    }
}

pub enum CliState {
    Continue,
    Stop,
}

struct Command {
    pub name: &'static str,
    pub help_text: &'static str,

    pub function: fn(&mut CliData, args: &[&str]) -> Result<CliState, CliError>,
}
