mod common;
mod info;
mod session;

use std::rc::Rc;
use std::path::Path;
use std::path::PathBuf;
use memory::{
    MI,
    flash_writer,
};
use std::time::Instant;

use probe::debug_probe::{
    DebugProbeInfo,
};

use std::fs;

use std::borrow;
use object;

use memmap;

use probe::target::m0::CortexDump;

use common::{
    with_device,
    with_dump,
    CliError,
};

use std::fs::File;
use std::io::Write;


use structopt::StructOpt;

use rustyline::Editor;

use capstone::{
    Capstone,
    Endian,
};
use capstone::prelude::*;
use capstone::arch::arm::ArchMode;

use session::Session;

use gimli;
use object::Object;

fn parse_hex(src: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(src, 16)
}

#[derive(StructOpt)]
#[structopt(
    name = "ST-Link CLI",
    about = "Get info about the connected ST-Links",
    author = "Noah Hüsser <yatekii@yatekii.ch>"
)]
enum CLI {
    /// List all connected ST-Links
    #[structopt(name = "list")]
    List {},
    /// Gets infos about the selected ST-Link
    #[structopt(name = "info")]
    Info {
        /// The number associated with the ST-Link to use
        n: usize,
    },
    /// Resets the target attached to the selected ST-Link
    #[structopt(name = "reset")]
    Reset {
        /// The number associated with the ST-Link to use
        n: usize,
        /// Whether the reset pin should be asserted or deasserted. If left open, just pulse it
        assert: Option<bool>,
    },
    #[structopt(name = "debug")]
    Debug {
        #[structopt(long, parse(from_os_str))]
        /// Dump file to debug
        dump: Option<PathBuf>,

        #[structopt(long, parse(from_os_str))]
        /// Binary to debug
        exe: Option<PathBuf>,

        // The number associated with the probe to use
        n: usize,
    },
    /// Dump memory from attached target
    #[structopt(name = "dump")]
    Dump {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
        /// The amount of memory (in words) to dump
        words: u32,
    },
    /// Download memory to attached target
    #[structopt(name = "download")]
    Download {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The path to the file to be downloaded to the flash
        path: String,
    },
    #[structopt(name = "erase")]
    Erase {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32
    },
    #[structopt(name = "trace")]
    Trace {
        /// The number associated with the ST-Link to use
        n: usize,
        /// The address of the memory to dump from the target (in hexadecimal without 0x prefix)
        #[structopt(parse(try_from_str = "parse_hex"))]
        loc: u32,
    },
}

fn main() {
    // Initialize the logging backend.
    pretty_env_logger::init();

    let matches = CLI::from_args();

    match matches {
        CLI::List {} => list_connected_devices(),
        CLI::Info { n } => crate::info::show_info_of_device(n).unwrap(),
        CLI::Reset { n, assert } => reset_target_of_device(n, assert).unwrap(),
        CLI::Debug { n, exe, dump } => debug(n, exe, dump).unwrap(),
        CLI::Dump { n, loc, words } => dump_memory(n, loc, words).unwrap(),
        CLI::Download { n, path } => download_program(n, path).unwrap(),
        CLI::Erase { n, loc } => erase_page(n, loc).unwrap(),
        CLI::Trace { n, loc } => trace_u32_on_target(n, loc).unwrap(),
    }
}

fn list_connected_devices() {
    let links = get_connected_devices();

    if links.len() > 0 {
        println!("The following devices were found:");
        links
            .iter()
            .enumerate()
            .for_each(|(num, link)| println!( "[{}]: {:?}", num, link));
    } else {
        println!("No devices were found.");
    }
}

fn dump_memory(n: usize, loc: u32, words: u32) -> Result<(), CliError> {
    with_device(n as usize, Box::new(probe::target::m0::M0::default()), |mut session| {
        let mut data = vec![0 as u32; words as usize];

        // Start timer.
        let instant = Instant::now();

        // let loc = 220 * 1024;

        session.probe.read_block32(loc, &mut data.as_mut_slice())?;
        // Stop timer.
        let elapsed = instant.elapsed();

        // Print read values.
        for word in 0..words {
            println!("Addr 0x{:08x?}: 0x{:08x}", loc + 4 * word, data[word as usize]);
        }
        // Print stats.
        println!("Read {:?} words in {:?}", words, elapsed);

        Ok(())
    })
}

fn download_program(n: usize, path: String) -> Result<(), CliError> {
    with_device(n as usize, Box::new(probe::target::m0::M0::default()), |mut session| {

        // Start timer.
        // let instant = Instant::now();

        // let NVMC = 0x4001E000;
        // let NVMC_CONFIG = NVMC + 0x504;
        // let WEN: u32 = 0x1;
        // let loc = 220 * 1024;

        // link.write(NVMC_CONFIG, WEN).or_else(|e| Err(Error::AccessPort(e)))?;
        // link.write(loc, 0x0u32).or_else(|e| Err(Error::AccessPort(e)))?;

        // // Stop timer.
        // let elapsed = instant.elapsed();

        flash_writer::download_hex(path, &mut session.probe, 1024)?;

        Ok(())

        // Ok(())
    })
}

#[allow(non_snake_case)]
fn erase_page(n: usize, loc: u32) -> Result<(), CliError> {

    with_device(n, Box::new(probe::target::m0::M0::default()), |mut session| {

        // TODO: Generic flash erase

        let NVMC = 0x4001E000;
        let NVMC_CONFIG = NVMC + 0x504;
        let NVMC_ERASEPAGE = NVMC + 0x508;
        let EEN: u32 = 0x2;

        session.probe.write32(NVMC_CONFIG, EEN)?;
        session.probe.write32(NVMC_ERASEPAGE, loc)?;

        Ok(())
    })
}

fn reset_target_of_device(n: usize, _assert: Option<bool>) -> Result<(), CliError> {
    with_device(n as usize, Box::new(probe::target::m0::M0::default()), |mut session| {
        //link.get_interface_mut::<DebugProbe>().unwrap().target_reset().or_else(|e| Err(Error::DebugProbe(e)))?;
        session.probe.target_reset()?;

        Ok(())
    })
}

fn trace_u32_on_target(n: usize, loc: u32) -> Result<(), CliError> {
    use std::io::prelude::*;
    use std::thread::sleep;
    use std::time::Duration;
    use scroll::{Pwrite};

    let mut xs = vec![];
    let mut ys = vec![];

    let start = Instant::now();

    with_device(n, Box::new(probe::target::m0::M0::default()), |mut session| {
        loop {
            // Prepare read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());

            // Read data.
            let value: u32 = session.probe.read32(loc)?;

            xs.push(instant);
            ys.push(value);

            // Send value to plot.py.
            // Unwrap is safe as there is always an stdin in our case!
            let mut buf = [0 as u8; 8];
            // Unwrap is safe!
            buf.pwrite(instant, 0).unwrap();
            buf.pwrite(value, 4).unwrap();
            std::io::stdout().write(&buf)?;

            std::io::stdout() .flush()?;

            // Schedule next read.
            let elapsed = start.elapsed();
            let instant = elapsed.as_secs() * 1000 + u64::from(elapsed.subsec_millis());
            let poll_every_ms = 50;
            let time_to_wait = poll_every_ms - instant % poll_every_ms;
            sleep(Duration::from_millis(time_to_wait));
        }
    })
}

fn get_connected_devices() -> Vec<DebugProbeInfo>{
    let mut links = daplink::tools::list_daplink_devices();
    links.extend(stlink::tools::list_stlink_devices());
    links
}

fn debug(n: usize, exe: Option<PathBuf>, dump: Option<PathBuf>) -> Result<(), CliError> {
    
    // try to load debug information
    let debug_data = exe.and_then(|p| fs::File::open(&p).ok() )
                        .and_then(|file| unsafe { memmap::Mmap::map(&file).ok() });


    //let file = fs::File::open(&path).unwrap();
    //let mmap = Rc::new(Box::new(unsafe { memmap::Mmap::map(&file).unwrap() }));

    
    let runner = |mut session| {
        let mut cs = Capstone::new()
            .arm()
            .mode(ArchMode::Thumb)
            .endian(Endian::Little)
            .build()
            .unwrap();



        let di = debug_data.as_ref().map( |mmap| DebugInfo::from_raw(&*mmap));
        
        /*
        if let Some(ref path) = exe {

            DebugInfo::from_file(path)
        } else {
            DebugInfo::none()
        }; */

        let mut rl = Editor::<()>::new();
        //rl.set_auto_add_history(true);

        loop {
            let readline = rl.readline(">> ");
            match readline {
                Ok(line) => {
                    let history_entry: &str = line.as_ref();
                    rl.add_history_entry(history_entry);
                    handle_line(&mut session, &mut cs, di.as_ref(), &line)?;
                },
                Err(e) => {
                    // Just quit for now
                    println!("Error handling input: {:?}", e);
                    return Ok(());
                }
            }
        }
    };

    match dump {
        None => with_device(n, Box::new(probe::target::m0::M0::default()), &runner),
        Some(p) => with_dump(&p, &runner),
    }

}


fn handle_line(session: &mut Session, cs: &mut Capstone, debug_info: Option<&DebugInfo>, line: &str) -> Result<(), CliError> {
    let mut command_parts = line.split_whitespace();

    let command = command_parts.next().unwrap();

    match command {
        "halt" => {
            let cpu_info = session.target.halt(&mut session.probe)?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);

            let mut code = [0u8;16*2];

            session.probe.read_block8(cpu_info.pc, &mut code)?;


            let instructions = cs.disasm_all(&code, cpu_info.pc as u64).unwrap();

            for i in instructions.iter() {
                println!("{}", i);
            }


            Ok(())
        },
        "run" => {
            session.target.run(&mut session.probe)?;
            Ok(())
        },
        "step" => {
            let cpu_info = session.target.step(&mut session.probe)?;
            println!("Core stopped at address 0x{:08x}", cpu_info.pc);
            Ok(())
        },
        "read" => {
            let address_str = command_parts.next().unwrap();
            let address = u32::from_str_radix(address_str, 16).unwrap();
            //println!("Would read from address 0x{:08x}", address);

            let val = session.probe.read32(address)?;
            println!("0x{:08x} = 0x{:08x}", address, val);
            Ok(())
        },
        "break" => {
            let address_str = command_parts.next().unwrap();
            let address = u32::from_str_radix(address_str, 16).unwrap();
            //println!("Would read from address 0x{:08x}", address);

            session.target.enable_breakpoints(&mut session.probe, true)?;
            session.target.set_breakpoint(&mut session.probe, address)?;

            Ok(())
        },
        "bt" => {
            use probe::target::m0::{PC, SP};
            let stack_pointer = session.target.read_core_reg(&mut session.probe, SP)?;
            let program_counter = session.target.read_core_reg(&mut session.probe, PC)?;

            println!("Current program counter: 0x{:08x}", program_counter);
            println!("Current stack pointer:   0x{:08x}", stack_pointer);

            if let Some(di) = debug_info {
                println!("Current function: {:?}", di.get_function_name(program_counter as u64, session));

                di.try_unwind(session, program_counter as u64);
            }


            Ok(())
        },
        "dump" => {
            // dump all relevant data, stack and regs for now..
            //
            // stack beginning -> assume beginning to be hardcoded


            let stack_top: u32 = 0x2000_0000 + 0x4_000;

            use probe::target::m0::{PC, SP, LR};

            let stack_bot: u32 = session.target.read_core_reg(&mut session.probe, SP)?;
            let pc: u32 = session.target.read_core_reg(&mut session.probe, PC)?;
            
            let mut stack = vec![0u8;(stack_top - stack_bot) as usize];

            session.probe.read_block8(stack_bot, &mut stack[..])?;

            let mut dump = CortexDump::new(stack);

            for i in 0..12 {
                dump.regs[i as usize] = session.target.read_core_reg(&mut session.probe, i.into())?;
            }

            dump.regs[13] = stack_bot;
            dump.regs[14] = session.target.read_core_reg(&mut session.probe, LR)?;
            dump.regs[15] = pc;

            let serialized = ron::ser::to_string(&dump).expect("Failed to serialize dump");

            let mut dump_file = File::create("dump.txt").expect("Failed to create file");

            dump_file.write_all(serialized.as_bytes()).expect("Failed to write dump file");


            Ok(())
        },
        "quit" => {
            Err(CliError::Quit)
        },
        _ => {
            println!("Unknown command '{}'", line);
            Ok(())
        }
    }
}

type DwarfReader = gimli::read::EndianRcSlice<gimli::LittleEndian>;

struct DebugInfo {
    //object: object::File<'a>,
    dwarf: gimli::Dwarf<DwarfReader>,
    frame_section: gimli::DebugFrame<DwarfReader>,
}

impl<'a> DebugInfo {

    fn from_raw(data: &'a [u8]) -> Self {

        let object = object::File::parse(data).unwrap();

        // Load a section and return as `Cow<[u8]>`.
        let load_section = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            let data = object
                .section_data_by_name(id.name())
                .unwrap_or(borrow::Cow::Borrowed(&[][..]));
            
            Ok(gimli::read::EndianRcSlice::new(Rc::from(&*data), gimli::LittleEndian))
        };
        // Load a supplementary section. We don't have a supplementary object file,
        // so always return an empty slice.
        let load_section_sup = |_| Ok(gimli::read::EndianRcSlice::new(Rc::from(&*borrow::Cow::Borrowed(&[][..])), gimli::LittleEndian));

        // Load all of the sections.
        let dwarf_cow = gimli::Dwarf::load(&load_section, &load_section_sup).unwrap();

        use gimli::Section;

        let frame_section = gimli::DebugFrame::load(load_section).unwrap();

        DebugInfo {
            //object,
            dwarf: dwarf_cow,
            frame_section,
        }
    }

    fn evaluate_frame_base(&self, session: &mut Session, expr: gimli::Expression<DwarfReader>, unit: &gimli::Unit<DwarfReader>) -> Option<u32> {
        let mut evaluation = expr.evaluation(unit.encoding());

        // go for evaluation
        let mut result = evaluation.evaluate().unwrap();

        loop {
            use gimli::EvaluationResult::*;

            result = match result {
                Complete => break,
                RequiresMemory { address, size, space, base_type } => {
                    let mut buff = vec![0u8;size as usize];
                    session.probe.read_block8(address as u32, &mut buff).expect("Failed to read memory");
                    match size {
                        1 => evaluation.resume_with_memory(gimli::Value::U8(buff[0])).unwrap(),
                        2 => {
                            let val: u16 = (buff[0] as u16) << 8 | (buff[1] as u16);
                            evaluation.resume_with_memory(gimli::Value::U16(val)).unwrap()
                        },
                        4 => {
                            let val: u32 = (buff[0] as u32) << 24 | (buff[1] as u32) << 16 | (buff[2] as u32) << 8 | (buff[3] as u32);
                            evaluation.resume_with_memory(gimli::Value::U32(val)).unwrap()
                        },
                        _ => unimplemented!(),
                    }
                },
                RequiresFrameBase => {
                    // not possible right now!
                    unimplemented!()
                },
                x => {
                    println!("{:?}", x);
                    unimplemented!()
                }
            }
        }

        let final_result = evaluation.result();

        assert!(final_result.len() > 0);

        let frame_base_loc = &final_result[0];


        get_piece_value(session, frame_base_loc)

    }

    fn get_function_name(&self, address: u64, session: &mut Session) -> Option<String> {
        // search line number information for this address

        let mut units = self.dwarf.units();
        
        while let Some(header) = units.next().unwrap() {
            let unit = match self.dwarf.unit(header) {
                Ok(unit) => unit,
                Err(_) => continue,
            };

            let mut ranges = self.dwarf.unit_ranges(&unit).unwrap();

            while let Some(range) = ranges.next().unwrap() {
                if (range.begin <= address) && (address < range.end) {
                    println!("Unit: {:?}", unit.name.as_ref().and_then(|raw_name| std::str::from_utf8(&raw_name).ok()).unwrap_or("<unknown>") );


                    // get function name

                    let ilnp = match unit.line_program.as_ref() {
                        Some(ilnp) => ilnp,
                        None => return None,
                    };

                    let mut rows = ilnp.clone().rows();

                    while let Some((header, row)) = rows.next_row().unwrap() {
                        //println!("Row address: 0x{:08x}", row.address());
                        if row.address() == address {
                            let file = row.file(header).unwrap().path_name();
                            let file_name_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file).unwrap()).unwrap().to_owned();

                            let file_dir = row.file(header).unwrap().directory(header).unwrap();
                            let file_dir_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file_dir).unwrap()).unwrap().to_owned();

                            println!("File {}, directory {:?}, on line {:?}, column {:?}", file_name_str, file_dir_str, row.line(), row.column());
                        } else {
                            let row_addr = row.address();

                            if ((row_addr as i64) - (address as i64)).abs() < 4 {
                                println!("Near miss: addr {:08x} - line info addr: {:08x}", address, row_addr);
                            }

                        }
                    }
                }
            }

            let mut entries_cursor = unit.entries();

            let mut current_depth = 0;

            let mut frame_base = None;

            'tag_loop: while let Some((depth, current)) = entries_cursor.next_dfs().unwrap() {
                current_depth += depth;

                // we are interested in functions / inlined functions


                match current.tag() {
                    gimli::DW_TAG_subprogram | gimli::DW_TAG_inlined_subroutine => {
                        let mut ranges = self.dwarf.die_ranges(&unit, &current).unwrap();

                        while let Some(ranges) = ranges.next().unwrap() {
                            if (ranges.begin <= address) && (address < ranges.end) {
                                // get framebase!
                                if let Some(frame_base_attr) = current.attr(gimli::DW_AT_frame_base).expect(" Failed to parse entry") {
                                    if let gimli::AttributeValue::Exprloc(e) = frame_base_attr.value() {
                                        frame_base = self.evaluate_frame_base(session, e, &unit)
                                    }
                                };

                                if let Some(fb) = frame_base {
                                    println!("Framebase: 0x:{:08x}", fb);
                                } else  {
                                    println!("No frambease :(");
                                }

                                if let Some(fn_name_attr) = current.attr(gimli::DW_AT_name).expect(" Failed to parse entry") {
                                    match fn_name_attr.value() {
                                        gimli::AttributeValue::DebugStrRef(fn_name_ref) => {
                                            let fn_name_raw = self.dwarf.string(fn_name_ref).unwrap();

                                            println!("Hooray! Function name: {:?}", std::str::from_utf8(&fn_name_raw).unwrap());
                                            print_all_attributes(session, frame_base, &self.dwarf, &unit, current, 0);
                                            break 'tag_loop;
                                        },
                                        _ => (),
                                    }
                                }
                            }
                        }
                    },
                    _ => (),
                };
            }

            let initial_depth = current_depth;

            while let Some((depth, current)) = entries_cursor.next_dfs().unwrap() {
                current_depth += depth;

                if current_depth <= initial_depth {
                    break;
                }

                let print_depth: usize = (current_depth-initial_depth) as usize;

                for _ in 0..(print_depth) {
                    print!("\t");
                }
                
                println!("Tag: {}", current.tag());
                print_all_attributes(session, frame_base, &self.dwarf, &unit, current, print_depth);
            }

        }

        None
    }


    fn try_unwind(&self, session: &mut Session, address: u64) {

        // read current registers
        let mut regs = [0u32;16];

        for i in 0..16 {
            regs[i as usize] = session.target.read_core_reg(&mut session.probe, i.into()).unwrap();
        }

        println!("Frame 0:");
        println!("PC at 0x{:08x}", address);
        for i in 0..16 {
            println!("Register r{}: {:08x}", i, regs[i]);
        }

        let mut lr = regs[14] & (!1);

        // Just assume its 16 bit thumb for now
        //lr -= 2;

        println!("Calling function: {:?}", self.get_function_name(lr as u64, session));


        let mut ctx = gimli::UninitializedUnwindContext::new();

        let bases = gimli::BaseAddresses::default();

        use gimli::UnwindSection;

        let unwind_info = self.frame_section.unwind_info_for_address(&bases, &mut ctx, address, gimli::DebugFrame::cie_from_offset).unwrap();

        println!("CFA: {:?}", unwind_info.cfa());

        for i in 0..16 {
            println!("Register r{}: {:?}", i, unwind_info.register(gimli::Register(i as u16)))
        }

        // generate previous registers

    }
}

fn get_piece_value(session: &mut Session, p: &gimli::Piece<DwarfReader>) -> Option<u32> {
    use gimli::Location;

    match &p.location {
        Location::Empty => None,
        Location::Address { address } => {
            println!("Piece in memory at 0x{:08x}! Not yet supported...", address);
            None
        },
        Location::Value { value } => {
            Some(value.to_u64(0xff_ff_ff_ff).unwrap()  as u32)
        },
        Location::Register { register } => {
            let val = session.target.read_core_reg(&mut session.probe, (register.0 as u8).into()).expect("Failed to read register from target");
            Some(val)
        },
        l => {
            unimplemented!("Location {:?} not implemented", l)
        }
    }

}

fn print_all_attributes(session: &mut Session, frame_base: Option<u32>, dwarf: &gimli::Dwarf<DwarfReader>, unit: &gimli::Unit<DwarfReader>, tag: &gimli::DebuggingInformationEntry<DwarfReader>, print_depth: usize) {
    let mut attrs = tag.attrs();


    while let Some(attr) = attrs.next().unwrap() {
        for _ in 0..(print_depth) {
            print!("\t");
        }
        print!("{}: ", attr.name()); //, attr.value());

        use gimli::AttributeValue::*;

        match attr.value() {
            Addr(a) => println!("0x{:08x}", a),
            DebugStrRef(_) => {
                let val = dwarf.attr_string(unit, attr.value()).unwrap();
                println!("{}", std::str::from_utf8(&val).unwrap());
            },
            Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                loop {
                    use gimli::EvaluationResult::*;

                    result = match result {
                        Complete => break,
                        RequiresMemory { address, size, space, base_type } => {
                            let mut buff = vec![0u8;size as usize];
                            session.probe.read_block8(address as u32, &mut buff).expect("Failed to read memory");
                            match size {
                                1 => evaluation.resume_with_memory(gimli::Value::U8(buff[0])).unwrap(),
                                2 => {
                                    let val: u16 = (buff[0] as u16) << 8 | (buff[1] as u16);
                                    evaluation.resume_with_memory(gimli::Value::U16(val)).unwrap()
                                },
                                4 => {
                                    let val: u32 = (buff[0] as u32) << 24 | (buff[1] as u32) << 16 | (buff[2] as u32) << 8 | (buff[3] as u32);
                                    evaluation.resume_with_memory(gimli::Value::U32(val)).unwrap()
                                },
                                _ => unimplemented!(),
                            }
                        },
                        RequiresFrameBase => {
                            evaluation.resume_with_frame_base(frame_base.unwrap() as u64).unwrap()
                        },
                        x => {
                            println!("{:?}", x);
                            unimplemented!()
                        }
                    }
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            },
            LocationListsRef(_) => {
                println!("LocationList");
            },
            DebugLocListsBase(_) => {
                println!(" LocationList");
            },
            DebugLocListsIndex(_) => {
                println!(" LocationList");
            },
            //_ => println!("{:?}", attr.value()),
            _ => println!("-"),
        }
    }
}