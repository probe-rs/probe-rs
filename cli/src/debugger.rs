use crate::common::CliError;

use probe_rs::{
    cores::CortexDump,
    coresight::memory::MI,
    debug::DebugInfo,
    session::{BreakpointId, Session},
};

use capstone::Capstone;

use std::fs::File;
use std::io::prelude::*;

pub struct DebugCli {
    commands: Vec<Command>,
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
                let cpu_info = cli_data
                    .session
                    .target
                    .core
                    .step(&mut cli_data.session.probe)?;
                println!("Core stopped at address 0x{:08x}", cpu_info.pc);

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "halt",
            help_text: "Stop the CPU",

            function: |cli_data, _args| {
                let cpu_info = cli_data
                    .session
                    .target
                    .core
                    .halt(&mut cli_data.session.probe)?;
                println!("Core stopped at address 0x{:08x}", cpu_info.pc);

                let mut code = [0u8; 16 * 2];

                cli_data.session.probe.read_block8(cpu_info.pc, &mut code)?;

                let instructions = cli_data
                    .capstone
                    .disasm_all(&code, u64::from(cpu_info.pc))
                    .unwrap();

                for i in instructions.iter() {
                    println!("{}", i);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "run",
            help_text: "Resume execution of the CPU",

            function: |cli_data, _args| {
                cli_data
                    .session
                    .target
                    .core
                    .run(&mut cli_data.session.probe)?;

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
                let address_str = args.get(0).ok_or(CliError::MissingArgument)?;

                let address = u32::from_str_radix(address_str, 16).unwrap();
                //println!("Would read from address 0x{:08x}", address);

                let num_words = args
                    .get(1)
                    .map(|c| c.parse::<usize>().unwrap())
                    .unwrap_or(1);

                let mut buff = vec![0u32; num_words];

                cli_data.session.probe.read_block32(address, &mut buff)?;

                for (offset, word) in buff.iter().enumerate() {
                    println!("0x{:08x} = 0x{:08x}", address + (offset * 4) as u32, word);
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "break",
            help_text: "Set a breakpoint at a specifc address",

            function: |cli_data, args| {
                let address_str = args.get(0).ok_or(CliError::MissingArgument)?;
                let address = u32::from_str_radix(address_str, 16).unwrap();
                //println!("Would read from address 0x{:08x}", address);

                let id = cli_data.session.set_hw_breakpoint(address)?;

                println!(
                    "Set new breakpoint at address {:#08x} with id {:?}",
                    address, id
                );

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "clear_break",
            help_text: "Clear a breakpoint",

            function: |cli_data, args| {
                let id_str = args.get(0).ok_or(CliError::MissingArgument)?;
                let id = usize::from_str_radix(id_str, 10).unwrap();
                //println!("Would read from address 0x{:08x}", address);

                cli_data
                    .session
                    .clear_hw_breakpoint(BreakpointId::new(id))?;

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "bt",
            help_text: "Show backtrace",

            function: |cli_data, _args| {
                let regs = cli_data.session.target.core.registers();
                let program_counter = cli_data
                    .session
                    .target
                    .core
                    .read_core_reg(&mut cli_data.session.probe, regs.PC)?;

                if let Some(di) = &cli_data.debug_info {
                    let frames = di.try_unwind(&mut cli_data.session, u64::from(program_counter));

                    for frame in frames {
                        println!("{}", frame);
                    }
                }

                Ok(CliState::Continue)
            },
        });

        cli.add_command(Command {
            name: "regs",
            help_text: "Show CPU register values",

            function: |cli_data, _args| {
                let mut regs = [0u32; 15];

                for i in 0..15 {
                    regs[i as usize] = cli_data
                        .session
                        .target
                        .core
                        .read_core_reg(&mut cli_data.session.probe, i.into())?;
                }

                for (i, val) in regs.iter().enumerate() {
                    println!("Register {}: {:#08x}", i, val);
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

                let stack_top: u32 = 0x2000_0000 + 0x4_000;

                let regs = cli_data.session.target.core.registers();

                let stack_bot: u32 = cli_data
                    .session
                    .target
                    .core
                    .read_core_reg(&mut cli_data.session.probe, regs.SP)?;
                let pc: u32 = cli_data
                    .session
                    .target
                    .core
                    .read_core_reg(&mut cli_data.session.probe, regs.PC)?;

                let mut stack = vec![0u8; (stack_top - stack_bot) as usize];

                cli_data
                    .session
                    .probe
                    .read_block8(stack_bot, &mut stack[..])?;

                let mut dump = CortexDump::new(stack_bot, stack);

                for i in 0..12 {
                    dump.regs[i as usize] = cli_data
                        .session
                        .target
                        .core
                        .read_core_reg(&mut cli_data.session.probe, i.into())?;
                }

                dump.regs[13] = stack_bot;
                dump.regs[14] = cli_data
                    .session
                    .target
                    .core
                    .read_core_reg(&mut cli_data.session.probe, regs.LR)?;
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
                cli_data
                    .session
                    .target
                    .core
                    .halt(&mut cli_data.session.probe)?;

                // Enable vector catch after reset (set bit 1 in DEMCR register)
                cli_data.session.probe.write32(0xE000_EDFC, 1)?;
                cli_data
                    .session
                    .target
                    .core
                    .reset(&mut cli_data.session.probe)?;

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

        if let Some(command) = command_parts.next() {
            // Special case for inbuilt help

            if command == "help" {
                println!("The following commands are available:");

                for cmd in &self.commands {
                    println!(" - {}", cmd.name);
                }

                return Ok(CliState::Continue);
            }

            let cmd = self.commands.iter().find(|c| c.name == command);

            if let Some(cmd) = cmd {
                let remaining_args: Vec<&str> = command_parts.collect();

                (cmd.function)(cli_data, &remaining_args)
            } else {
                println!("Unknown command '{}'", command);
                println!("Enter 'help' for a list of commands");

                Ok(CliState::Continue)
            }
        } else {
            Ok(CliState::Continue)
        }
    }
}

pub struct CliData {
    pub session: Session,
    pub debug_info: Option<DebugInfo>,
    pub capstone: Capstone,
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
