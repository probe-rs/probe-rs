use crate::common::CliError;

use capstone::Capstone;
use num_traits::Num;
use probe_rs::architecture::arm::Dump;
use probe_rs::debug::{DebugInfo, Registers, VariableName};
use probe_rs::{Core, CoreRegisterAddress, MemoryInterface};
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

                /*
                let instructions = cli_data
                    .capstone
                    .disasm_all(&code, u64::from(cpu_info.pc))
                    .unwrap();

                for i in instructions.iter() {
                    println!("{}", i);
                }
                 */

                for (offset, instruction) in code.iter().enumerate() {
                    println!(
                        "{:#010x}: {:010x}",
                        cpu_info.pc + offset as u32,
                        instruction
                    );
                }

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
                    let pc = cli_data
                        .core
                        .read_core_reg(cli_data.core.registers().program_counter())?;
                    println!("Core halted at address {:#010x}", pc);

                    // determine if the target is handling an interupt

                    // TODO: Proper address
                    let xpsr = cli_data.core.read_core_reg(
                        16,
                    )?;

                    println!("XPSR: {:#010x}", xpsr);

                    let exception_number = xpsr & 0xff;

                    if exception_number != 0 {
                        println!("Currently handling exception {}", exception_number);

                        if exception_number == 3 {
                            println!("Hard Fault!");


                            let return_address = cli_data.core.read_core_reg(cli_data.core.registers().return_address())?;

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
                    println!("0x{:08x} = 0x{:08x}", address + (offset * 4) as u32, word);
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
                        let program_counter =
                            cli_data.core.read_core_reg(regs.program_counter())?;

                        if let Some(di) = &mut cli_data.debug_info {
                            halted_state.stack_frames = di
                                .unwind(&mut cli_data.core, u64::from(program_counter))
                                .unwrap();

                            halted_state.frame_indices = halted_state
                                .stack_frames
                                .iter()
                                .map(|sf| sf.id as i64)
                                .collect();

                            for (i, frame) in halted_state.stack_frames.iter().enumerate() {
                                print!("Frame {}: {} @ {:#010x}", i, frame.function_name, frame.pc);

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

                for register in register_file.registers() {
                    let value = cli_data.core.read_core_reg(register)?;

                    println!("{}: {:#010x}", register.name(), value)
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
                            if let Some(current_frame) = halted_state.get_current_frame() {
                                current_frame
                            } else {
                                println!("StackFrame not found.");
                                return Ok(CliState::Continue);
                            };

                        let local_variable_cache = if let Some(local_variable_cache) =
                            current_frame.local_variables.as_mut()
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
                                    "{}: {}",
                                    child.name,
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

                cli_data.core.read(stack_bot, &mut stack[..])?;

                let mut dump = Dump::new(stack_bot, stack);

                for i in 0..12 {
                    dump.regs[i as usize] =
                        cli_data
                            .core
                            .read_core_reg(Into::<CoreRegisterAddress>::into(i))?;
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
    pub capstone: Capstone,

    state: DebugState,
}

impl<'p> CliData<'p> {
    pub fn new(
        mut core: Core<'p>,
        debug_info: Option<DebugInfo>,
        capstone: Capstone,
    ) -> Result<CliData, CliError> {
        let status = core.status()?;

        // TODO: In halted state we should get the backtrace here.
        let debug_state = match status {
            probe_rs::CoreStatus::Halted(_) => {
                let registers = Registers::from_core(&mut core);

                DebugState::Halted(HaltedState {
                    program_counter: registers.get_program_counter().unwrap_or_default(),
                    current_frame: 0,
                    frame_indices: vec![1],
                    stack_frames: vec![],
                })
            }
            _other => DebugState::Running,
        };

        // TODO: Find initial state
        Ok(CliData {
            core,
            debug_info,
            capstone,
            state: debug_state,
        })
    }

    pub fn print_state(&mut self) -> Result<(), CliError> {
        match self.state {
            DebugState::Running => println!("Core is running."),
            DebugState::Halted(ref mut halted_state) => {
                let pc = halted_state.program_counter;
                if let Some(current_stack_frame) = halted_state.get_current_frame() {
                    println!(
                        "Frame {}: {} @ {:#010x}",
                        current_stack_frame, current_stack_frame.function_name, pc,
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

struct HaltedState {
    program_counter: u32,
    current_frame: usize,
    frame_indices: Vec<i64>,
    stack_frames: Vec<probe_rs::debug::StackFrame>,
}

impl HaltedState {
    fn get_current_frame(&mut self) -> Option<&mut probe_rs::debug::StackFrame> {
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
