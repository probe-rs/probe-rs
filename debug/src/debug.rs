use std::path::PathBuf;
use std::borrow;
use std::rc::Rc;

use object::read::Object;
use memory::MI;

use log::debug;

use crate::session::Session;
use crate::*;

#[derive(Debug, Copy, Clone)]
pub enum ColumnType {
    LeftEdge,
    Column(u64)
}

impl From<gimli::ColumnType> for ColumnType {
    fn from(column: gimli::ColumnType) -> Self {
        match column {
            gimli::ColumnType::LeftEdge => ColumnType::LeftEdge,
            gimli::ColumnType::Column(c) => ColumnType::Column(c),
        }
    }
}

#[derive(Debug)]
pub struct StackFrame {
    pub id: u64,
    pub function_name: String,
    pub source_location: Option<SourceLocation>,
    registers: Registers,
    pc: u32,
    variables: Vec<Variable>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "{}: {}", self.id, self.function_name)?;
        if let Some(si) = &self.source_location {
            write!(
                f,
                "\t{}/{}",
                si.directory.as_ref().map(|p| p.to_string_lossy()).unwrap_or(std::borrow::Cow::from("<unknown dir>")), 
                si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
            )?;

            if si.column.is_some() && si.line.is_some() {
                match si.column.unwrap() {
                    ColumnType::Column(c) => write!(f, ":{}:{}", si.line.unwrap(), c)?,
                    ColumnType::LeftEdge => write!(f, ":{}", si.line.unwrap())?,
                }
            }
        }

        write!(f, "\n")?;
        writeln!(f, "\tVariables:")?;

        for variable in &self.variables {
            writeln!(f, "\t\t{}", variable.name)?;
        }
        write!(f, "")
    }
}

#[derive(Debug, Clone)]
struct Registers([Option<u32>; 16]);

impl Registers {
    pub fn from_session(session: &mut Session) -> Self {
        let mut registers = Registers([None;16]);
        for i in 0..16 {
            registers[i as usize] = Some(session.target.read_core_reg(&mut session.probe, i.into()).unwrap());
        }
        registers
    }
    
    pub fn get_call_frame_address(&self) -> Option<u32> {
        self.0[13]
    }

    pub fn set_call_frame_address(&mut self, value: Option<u32>) {
        self.0[13] = value;
    }
}

impl std::ops::Index<usize> for Registers {
    type Output = Option<u32>;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl std::ops::IndexMut<usize> for Registers {
    fn index_mut<'a>(&'a mut self, index: usize) -> &'a mut Self::Output {
        &mut self.0[index]
    }
}

impl std::ops::Index<std::ops::Range<usize>> for Registers {
    type Output = [Option<u32>];

    fn index(&self, index: std::ops::Range<usize>) -> &Self::Output {
        &self.0[index]
    }
}

impl std::ops::IndexMut<std::ops::Range<usize>> for Registers {
    fn index_mut<'a>(&'a mut self, index: std::ops::Range<usize>) -> &'a mut Self::Output {
        &mut self.0[index]
    }
}

#[derive(Debug)]
pub struct SourceLocation {
    pub line: Option<u64>,
    pub column: Option<ColumnType>,

    pub file: Option<String>,
    pub directory: Option<PathBuf>,
}

pub struct StackFrameIterator<'a> {
    debug_info: &'a DebugInfo,
    session: &'a mut Session,
    frame_count: u64,
    pc: Option<u64>,
    registers: Registers,
}

impl<'a> StackFrameIterator<'a> {
    pub fn new(debug_info: &'a DebugInfo, session: &'a mut Session, address: u64) -> Self {
        let registers = Registers::from_session(session);
        let pc = address;

        Self  {
            debug_info,
            session,
            frame_count: 0,
            pc: Some(pc),
            registers,
        }
    }
}

impl<'a> Iterator for StackFrameIterator<'a> {
    type Item = StackFrame;

    fn next(&mut self) -> Option<Self::Item> {
        use gimli::UnwindSection;
        let mut ctx = gimli::UninitializedUnwindContext::new();
        let bases = gimli::BaseAddresses::default();

        let pc = match self.pc {
            Some(pc) => pc,
            None => { return None; }
        };


        let unwind_info = self.debug_info.frame_section.unwind_info_for_address(
            &bases,
            &mut ctx,
            pc,
            gimli::DebugFrame::cie_from_offset
        ).unwrap();

        let current_cfa = match unwind_info.cfa() {
            gimli::CfaRule::RegisterAndOffset { register, offset } => {
                let reg_val = self.registers[register.0 as usize];
                
                Some(((reg_val.unwrap() as i64) + offset) as u32)
            },
            gimli::CfaRule::Expression(_) => unimplemented!()
        };

        // generate previous registers
        for i in 0..16 {
            if i == 13 {
                continue;
            }

            use gimli::read::RegisterRule::*;

            self.registers[i] = match unwind_info.register(gimli::Register(i as u16)) {
                Undefined => None,
                SameValue => self.registers[i],
                Offset(o) => {
                    let addr = (current_cfa.unwrap() as i64) + o;
                    let mut buff = [0u8;4];
                    self.session.target.read_block8(&mut self.session.probe, addr as u32, &mut buff).unwrap();

                    let val = u32::from_le_bytes(buff);

                    Some(val)
                },
                _ => unimplemented!()
            }
        }

        self.registers.set_call_frame_address(current_cfa);

        let unit_info = self.debug_info.get_unit_info();

        let unknown_function = format!("<unknown_function_{}>", self.frame_count).to_string();

        let (function_name, variables) = if let Some(ui) = unit_info {
            if let Some(die_cursor_state) = &mut ui.get_function_die(pc) {
                dbg!(die_cursor_state.depth);
                let function_name = ui
                    .get_function_name(&die_cursor_state.function_die)
                    .unwrap_or(unknown_function);
                
                let variables = ui.get_variables(die_cursor_state);

                (function_name, variables)
            } else {
                (unknown_function, vec![])
            }
        } else {
            (unknown_function, vec![])
        };

        let return_frame = Some(StackFrame {
            id: self.frame_count,
            function_name,
            source_location: self.debug_info.get_source_location(pc),
            registers: self.registers.clone(),
            pc: pc as u32,
            variables,
        });

        self.frame_count += 1;

        // Next function is where our current return register is pointing to.
        // We just have to remove the lowest bit (indicator for Thumb mode).
        self.pc = self.registers[14].map( |pc| (pc &!1) as u64);

        return return_frame;
    }
}

type DwarfReader = gimli::read::EndianRcSlice<gimli::LittleEndian>;
type FunctionDie<'a, 'u> = gimli::DebuggingInformationEntry<'a, 'u, gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>, usize>;
type EntriesCursor<'a, 'u> = gimli::EntriesCursor<'a, 'u, gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;

pub struct DebugInfo {
    dwarf: gimli::Dwarf<DwarfReader>,
    frame_section: gimli::DebugFrame<DwarfReader>,
    current_unit: Option<gimli::Unit<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>, usize>>,
}

impl DebugInfo {
    pub fn from_raw<'a> (data: &'a [u8]) -> Self {

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
            current_unit: None,
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

    fn get_source_location(&self, address: u64) -> Option<SourceLocation> {
        let mut units = self.dwarf.units();
        
        while let Some(header) = units.next().unwrap() {
            let unit = match self.dwarf.unit(header) {
                Ok(unit) => unit,
                Err(_) => continue,
            };

            let mut ranges = self.dwarf.unit_ranges(&unit).unwrap();

            while let Some(range) = ranges.next().unwrap() {
                if (range.begin <= address) && (address < range.end) {
                    //debug!("Unit: {:?}", unit.name.as_ref().and_then(|raw_name| std::str::from_utf8(&raw_name).ok()).unwrap_or("<unknown>") );


                    // get function name

                    let ilnp = match unit.line_program.as_ref() {
                        Some(ilnp) => ilnp,
                        None => return None,
                    };

                    let (program, sequences) = ilnp.clone().sequences().unwrap();

                    // normalize address
                    let mut target_seq = None;

                    for seq in sequences {
                        //println!("Seq 0x{:08x} - 0x{:08x}", seq.start, seq.end);
                        if (seq.start <= address) && (address < seq.end) {
                            target_seq = Some(seq);                            
                            break;
                        }
                    }

                    if target_seq.is_none() {
                        return None;
                    }

                    let mut previous_row: Option<gimli::LineRow> = None;

                    let mut rows = program.resume_from(target_seq.as_ref().expect("Sequence not found"));

                    while let Some((header, row)) = rows.next_row().unwrap() {
                        //println!("Row address: 0x{:08x}", row.address());
                        if row.address() == address {
                            let file = row.file(header).unwrap().path_name();
                            let file_name_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file).unwrap()).unwrap().to_owned();

                            let file_dir = row.file(header).unwrap().directory(header).unwrap();
                            let file_dir_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file_dir).unwrap()).unwrap().to_owned();

                            return Some(SourceLocation {
                                line: row.line(),
                                column: Some(row.column().into()),
                                file: file_name_str.into(),
                                directory: Some(file_dir_str.into()),
                            })
                        } else {
                            if (row.address() > address) && previous_row.is_some() {
                                let row = previous_row.unwrap();

                                let file = row.file(header).unwrap().path_name();
                                let file_name_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file).unwrap()).unwrap().to_owned();

                                let file_dir = row.file(header).unwrap().directory(header).unwrap();
                                let file_dir_str = std::str::from_utf8(&self.dwarf.attr_string(&unit, file_dir).unwrap()).unwrap().to_owned();

                                return Some(SourceLocation {
                                    line: row.line(),
                                    column: Some(row.column().into()),
                                    file: file_name_str.into(),
                                    directory: Some(file_dir_str.into()),
                                })
                            }
                        }
                        previous_row = Some(row.clone());
                    }
                }
            }
        }
        None

    }

    fn get_unit_info(&self) -> Option<UnitInfo> {
        let mut units = self.dwarf.units();
        
        if let Ok(Some(header)) = units.next() {
            let unit = match self.dwarf.unit(header) {
                Ok(unit) => unit,
                Err(_) => return None,
            };
            return Some(UnitInfo {
                debug_info: self,
                unit,
            })
        }
        None
    }

    pub fn try_unwind<'b>(&'b self, session: &'b mut Session, address: u64) -> StackFrameIterator<'b> {
        StackFrameIterator::new(&self, session, address)
    }
}

pub struct DieCursorState<'a, 'u> {
    entries_cursor: EntriesCursor<'a, 'u>,
    depth: isize,
    function_die: FunctionDie<'a, 'u>,
}

pub struct UnitInfo<'a> {
    debug_info: &'a DebugInfo,
    unit: gimli::Unit<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>, usize>,
}

impl<'a> UnitInfo<'a> {
    fn get_function_die<'b>(&'b self, address: u64) -> Option<DieCursorState<'b, 'b>> {
        let mut entries_cursor = self.unit.entries();
        dbg!(address);

        while let Some((depth, current)) = entries_cursor.next_dfs().unwrap() {
            match current.tag() {
                gimli::DW_TAG_subprogram | gimli::DW_TAG_inlined_subroutine => {
                    let mut ranges = self.debug_info.dwarf.die_ranges(&self.unit, &current).unwrap();

                    while let Some(ranges) = ranges.next().unwrap() {
                        dbg!(ranges);
                        if (ranges.begin <= address) && (address < ranges.end) {
                            return Some(DieCursorState {
                                depth,
                                function_die: current.clone(),
                                entries_cursor,
                            });
                        }
                    }
                },
                _ => (),
            };
        }
        None
    }

    fn get_function_name(&self, function_die: &FunctionDie) -> Option<String> {
        if let Some(fn_name_attr) = function_die.attr(gimli::DW_AT_name).expect(" Failed to parse entry") {
            match fn_name_attr.value() {
                gimli::AttributeValue::DebugStrRef(fn_name_ref) => {
                    let fn_name_raw = self.debug_info.dwarf.string(fn_name_ref).unwrap();

                    return Some(String::from_utf8_lossy(&fn_name_raw).to_string());
                },
                _ => (),
            }
        }

        None
    }

    fn get_variables(&self, die_cursor_state: &mut DieCursorState) -> Vec<Variable> {
        let mut variables = vec![];

        while let Some((depth, current)) = die_cursor_state.entries_cursor.next_dfs().unwrap() {
            println!("kakakdaksd: {}, {}", depth, die_cursor_state.depth);
            if depth != die_cursor_state.depth {
                break;
            }
            match current.tag() {
                gimli::DW_TAG_variable => {
                    if let Some(fn_name_attr) = die_cursor_state.function_die.attr(gimli::DW_AT_name).expect(" Failed to parse entry") {
                        match fn_name_attr.value() {
                            gimli::AttributeValue::DebugStrRef(fn_name_ref) => {
                                let fn_name_raw = self.debug_info.dwarf.string(fn_name_ref).unwrap();

                                variables.push(Variable {
                                    name: String::from_utf8_lossy(&fn_name_raw).to_string(),
                                });
                            },
                            _ => (),
                        }
                    }
                },
                _ => (),
            };
        }

        variables
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