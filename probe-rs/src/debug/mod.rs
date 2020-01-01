pub mod typ;
pub mod variable;

pub use typ::*;
pub use variable::*;

use std::borrow;
use std::path::PathBuf;
use std::rc::Rc;

use crate::coresight::memory::MI;
use object::read::Object;

use crate::session::Session;

#[derive(Debug, Copy, Clone)]
pub enum ColumnType {
    LeftEdge,
    Column(u64),
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
    pub variables: Vec<Variable>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "{}: {}", self.id, self.function_name)?;
        if let Some(si) = &self.source_location {
            write!(
                f,
                "\t{}/{}",
                si.directory
                    .as_ref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_else(|| std::borrow::Cow::from("<unknown dir>")),
                si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
            )?;

            if si.column.is_some() && si.line.is_some() {
                match si.column.unwrap() {
                    ColumnType::Column(c) => write!(f, ":{}:{}", si.line.unwrap(), c)?,
                    ColumnType::LeftEdge => write!(f, ":{}", si.line.unwrap())?,
                }
            }
        }

        writeln!(f)?;
        writeln!(f, "\tVariables:")?;

        for variable in &self.variables {
            writeln!(
                f,
                "\t\t{}: {}:{} = 0x{:08x}",
                variable.name, variable.file, variable.line, variable.value
            )?;
        }
        write!(f, "")
    }
}

#[derive(Debug, Clone)]
struct Registers([Option<u32>; 16]);

impl Registers {
    pub fn from_session(session: &mut Session) -> Self {
        let mut registers = Registers([None; 16]);
        for i in 0..16 {
            registers[i as usize] = Some(
                session
                    .target
                    .core
                    .read_core_reg(&mut session.probe, i.into())
                    .unwrap(),
            );
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
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
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

        Self {
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
            None => {
                return None;
            }
        };

        let unwind_info = self
            .debug_info
            .frame_section
            .unwind_info_for_address(&bases, &mut ctx, pc, gimli::DebugFrame::cie_from_offset)
            .unwrap();

        let current_cfa = match unwind_info.cfa() {
            gimli::CfaRule::RegisterAndOffset { register, offset } => {
                let reg_val = self.registers[register.0 as usize];

                Some((i64::from(reg_val.unwrap()) + offset) as u32)
            }
            gimli::CfaRule::Expression(_) => unimplemented!(),
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
                    let addr = i64::from(current_cfa.unwrap()) + o;
                    let mut buff = [0u8; 4];
                    self.session
                        .target
                        .core
                        .read_block8(&mut self.session.probe, addr as u32, &mut buff)
                        .unwrap();

                    let val = u32::from_le_bytes(buff);

                    Some(val)
                }
                _ => unimplemented!(),
            }
        }

        self.registers.set_call_frame_address(current_cfa);

        let return_frame = Some(self.debug_info.get_stackframe_info(
            &mut self.session,
            pc,
            self.frame_count,
            self.registers.clone(),
        ));

        self.frame_count += 1;

        // Next function is where our current return register is pointing to.
        // We just have to remove the lowest bit (indicator for Thumb mode).
        self.pc = self.registers[14].map(|pc| u64::from(pc & !1));

        return_frame
    }
}

type R = gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>;
type DwarfReader = gimli::read::EndianRcSlice<gimli::LittleEndian>;
type FunctionDie<'a, 'u> = gimli::DebuggingInformationEntry<
    'a,
    'u,
    gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>,
    usize,
>;
type EntriesCursor<'a, 'u> =
    gimli::EntriesCursor<'a, 'u, gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;
type UnitIter =
    gimli::CompilationUnitHeadersIter<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;

/// Debug information which is parsed from DWARF debugging information.
pub struct DebugInfo {
    dwarf: gimli::Dwarf<DwarfReader>,
    frame_section: gimli::DebugFrame<DwarfReader>,
}

impl DebugInfo {
    pub fn from_raw(data: &[u8]) -> Self {
        let object = object::File::parse(data).unwrap();

        // Load a section and return as `Cow<[u8]>`.
        let load_section = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            let data = object
                .section_data_by_name(id.name())
                .unwrap_or_else(|| borrow::Cow::Borrowed(&[][..]));

            Ok(gimli::read::EndianRcSlice::new(
                Rc::from(&*data),
                gimli::LittleEndian,
            ))
        };
        // Load a supplementary section. We don't have a supplementary object file,
        // so always return an empty slice.
        let load_section_sup = |_| {
            Ok(gimli::read::EndianRcSlice::new(
                Rc::from(&*borrow::Cow::Borrowed(&[][..])),
                gimli::LittleEndian,
            ))
        };

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

                    target_seq.as_ref()?;

                    let mut previous_row: Option<gimli::LineRow> = None;

                    let mut rows =
                        program.resume_from(target_seq.as_ref().expect("Sequence not found"));

                    while let Some((header, row)) = rows.next_row().unwrap() {
                        //println!("Row address: 0x{:08x}", row.address());
                        if row.address() == address {
                            let file = row.file(header).unwrap().path_name();
                            let file_name_str =
                                std::str::from_utf8(&self.dwarf.attr_string(&unit, file).unwrap())
                                    .unwrap()
                                    .to_owned();

                            let file_dir = row.file(header).unwrap().directory(header).unwrap();
                            let file_dir_str = std::str::from_utf8(
                                &self.dwarf.attr_string(&unit, file_dir).unwrap(),
                            )
                            .unwrap()
                            .to_owned();

                            return Some(SourceLocation {
                                line: row.line(),
                                column: Some(row.column().into()),
                                file: file_name_str.into(),
                                directory: Some(file_dir_str.into()),
                            });
                        } else if (row.address() > address) && previous_row.is_some() {
                            let row = previous_row.unwrap();

                            let file = row.file(header).unwrap().path_name();
                            let file_name_str =
                                std::str::from_utf8(&self.dwarf.attr_string(&unit, file).unwrap())
                                    .unwrap()
                                    .to_owned();

                            let file_dir = row.file(header).unwrap().directory(header).unwrap();
                            let file_dir_str = std::str::from_utf8(
                                &self.dwarf.attr_string(&unit, file_dir).unwrap(),
                            )
                            .unwrap()
                            .to_owned();

                            return Some(SourceLocation {
                                line: row.line(),
                                column: Some(row.column().into()),
                                file: file_name_str.into(),
                                directory: Some(file_dir_str.into()),
                            });
                        }
                        previous_row = Some(*row);
                    }
                }
            }
        }
        None
    }

    fn get_units(&self) -> UnitIter {
        self.dwarf.units()
    }

    fn get_next_unit_info(&self, units: &mut UnitIter) -> Option<UnitInfo> {
        while let Ok(Some(header)) = units.next() {
            if let Ok(unit) = self.dwarf.unit(header) {
                return Some(UnitInfo {
                    debug_info: self,
                    unit,
                });
            };
        }
        None
    }

    fn get_stackframe_info(
        &self,
        session: &mut Session,
        address: u64,
        frame_count: u64,
        registers: Registers,
    ) -> StackFrame {
        let mut units = self.get_units();
        let unknown_function = format!("<unknown_function_{}>", frame_count);
        while let Some(unit_info) = self.get_next_unit_info(&mut units) {
            if let Some(die_cursor_state) = &mut unit_info.get_function_die(address) {
                let function_name = unit_info
                    .get_function_name(&die_cursor_state.function_die)
                    .unwrap_or(unknown_function);

                let variables = unit_info.get_variables(
                    session,
                    die_cursor_state,
                    u64::from(registers.get_call_frame_address().unwrap()),
                );

                // dbg!(&variables);

                return StackFrame {
                    id: frame_count,
                    function_name,
                    source_location: self.get_source_location(address),
                    registers,
                    pc: address as u32,
                    variables,
                };
            }
        }

        StackFrame {
            id: frame_count,
            function_name: unknown_function,
            source_location: self.get_source_location(address),
            registers,
            pc: address as u32,
            variables: vec![],
        }
    }

    pub fn try_unwind<'b>(
        &'b self,
        session: &'b mut Session,
        address: u64,
    ) -> StackFrameIterator<'b> {
        StackFrameIterator::new(&self, session, address)
    }
}

pub struct DieCursorState<'a, 'u> {
    entries_cursor: EntriesCursor<'a, 'u>,
    _depth: isize,
    function_die: FunctionDie<'a, 'u>,
}

pub struct UnitInfo<'a> {
    debug_info: &'a DebugInfo,
    unit: gimli::Unit<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>, usize>,
}

impl<'a> UnitInfo<'a> {
    fn get_function_die(&self, address: u64) -> Option<DieCursorState> {
        let mut entries_cursor = self.unit.entries();

        while let Some((depth, current)) = entries_cursor.next_dfs().unwrap() {
            match current.tag() {
                gimli::DW_TAG_subprogram | gimli::DW_TAG_inlined_subroutine => {
                    let mut ranges = self
                        .debug_info
                        .dwarf
                        .die_ranges(&self.unit, &current)
                        .unwrap();

                    while let Some(ranges) = ranges.next().unwrap() {
                        if (ranges.begin <= address) && (address < ranges.end) {
                            return Some(DieCursorState {
                                _depth: depth,
                                function_die: current.clone(),
                                entries_cursor,
                            });
                        }
                    }
                }
                _ => (),
            };
        }
        None
    }

    fn get_function_name(&self, function_die: &FunctionDie) -> Option<String> {
        if let Some(fn_name_attr) = function_die
            .attr(gimli::DW_AT_name)
            .expect(" Failed to parse entry")
        {
            if let gimli::AttributeValue::DebugStrRef(fn_name_ref) = fn_name_attr.value() {
                let fn_name_raw = self.debug_info.dwarf.string(fn_name_ref).unwrap();

                return Some(String::from_utf8_lossy(&fn_name_raw).to_string());
            }
        }

        None
    }

    fn expr_to_piece(
        &self,
        session: &mut Session,
        expression: gimli::Expression<R>,
        frame_base: u64,
    ) -> Vec<gimli::Piece<R, usize>> {
        let mut evaluation = expression.evaluation(self.unit.encoding());

        // go for evaluation
        let mut result = evaluation.evaluate().unwrap();

        loop {
            use gimli::EvaluationResult::*;

            result = match result {
                Complete => break,
                RequiresMemory { address, size, .. } => {
                    let mut buff = vec![0u8; size as usize];
                    session
                        .probe
                        .read_block8(address as u32, &mut buff)
                        .expect("Failed to read memory");
                    match size {
                        1 => evaluation
                            .resume_with_memory(gimli::Value::U8(buff[0]))
                            .unwrap(),
                        2 => {
                            let val = (u16::from(buff[0]) << 8) | (u16::from(buff[1]) as u16);
                            evaluation
                                .resume_with_memory(gimli::Value::U16(val))
                                .unwrap()
                        }
                        4 => {
                            let val = (u32::from(buff[0]) << 24)
                                | (u32::from(buff[1]) << 16)
                                | (u32::from(buff[2]) << 8)
                                | u32::from(buff[3]);
                            evaluation
                                .resume_with_memory(gimli::Value::U32(val))
                                .unwrap()
                        }
                        _ => unimplemented!(),
                    }
                }
                RequiresFrameBase => evaluation.resume_with_frame_base(frame_base).unwrap(),
                x => {
                    println!("{:?}", x);
                    unimplemented!()
                }
            }
        }

        evaluation.result()
    }

    fn get_variables(
        &self,
        session: &mut Session,
        die_cursor_state: &mut DieCursorState,
        frame_base: u64,
    ) -> Vec<Variable> {
        let mut variables = vec![];

        while let Some((depth, current)) = die_cursor_state.entries_cursor.next_dfs().unwrap() {
            if depth != 0 && depth != 1 {
                break;
            }
            if let gimli::DW_TAG_variable = current.tag() {
                let mut variable = Variable {
                    name: String::new(),
                    file: String::new(),
                    line: u64::max_value(),
                    value: 0,
                    ..Default::default()
                };
                let mut attrs = current.attrs();
                while let Ok(Some(attr)) = attrs.next() {
                    match attr.name() {
                        gimli::DW_AT_name => {
                            variable.name = extract_name(&self.debug_info, attr.value())
                                .unwrap_or_else(|| "<undefined>".to_string());
                        }
                        gimli::DW_AT_decl_file => {
                            variable.file =
                                extract_file(&self.debug_info, &self.unit, attr.value())
                                    .unwrap_or_else(|| "<undefined>".to_string());
                        }
                        gimli::DW_AT_decl_line => {
                            variable.line = extract_line(&self.debug_info, attr.value())
                                .unwrap_or_else(u64::max_value);
                        }
                        gimli::DW_AT_type => {
                            variable.typ =
                                extract_type(&self, attr.value()).unwrap_or_else(|| Type {
                                    name: "<undefined>".to_string(),
                                    named_children: None,
                                    indexed_children: None,
                                });
                        }
                        gimli::DW_AT_location => {
                            variable.value =
                                extract_location(&self, session, frame_base, attr.value())
                                    .unwrap_or_else(u64::max_value);
                        }
                        _ => (),
                    }
                }
                variables.push(variable);
            };
        }

        variables
    }
}

fn extract_location(
    unit_info: &UnitInfo,
    session: &mut Session,
    frame_base: u64,
    attribute_value: gimli::AttributeValue<R>,
) -> Option<u64> {
    match attribute_value {
        gimli::AttributeValue::Exprloc(expression) => {
            let piece = unit_info.expr_to_piece(session, expression, frame_base);

            let value = get_piece_value(session, &piece[0]);
            value.map(u64::from)
        }
        _ => None,
    }
}

fn extract_type(unit_info: &UnitInfo, attribute_value: gimli::AttributeValue<R>) -> Option<Type> {
    match attribute_value {
        gimli::AttributeValue::UnitRef(unit_ref) => {
            if let Ok(mut tree) = unit_info.unit.entries_tree(Some(unit_ref)) {
                let node = tree.root().unwrap();

                // Examine the entry attributes.
                let entry = node.entry();
                if let gimli::DW_TAG_structure_type = entry.tag() {
                    let type_name = extract_name(
                        &unit_info.debug_info,
                        entry.attr(gimli::DW_AT_name).unwrap().unwrap().value(),
                    );
                    let mut named_children = std::collections::HashMap::new();

                    let mut children = node.children();
                    while let Ok(Some(child)) = children.next() {
                        // Recursively process a child.
                        let entry = child.entry();
                        if let gimli::DW_TAG_member = entry.tag() {
                            let member_name = extract_name(
                                &unit_info.debug_info,
                                entry.attr(gimli::DW_AT_name).unwrap().unwrap().value(),
                            );
                            named_children.insert(
                                member_name.unwrap(),
                                extract_type(
                                    unit_info,
                                    entry.attr(gimli::DW_AT_type).unwrap().unwrap().value(),
                                )
                                .unwrap(),
                            );
                        };
                    }

                    return Some(Type {
                        name: type_name.unwrap_or_else(|| "<unnamed type>".to_string()),
                        named_children: Some(named_children),
                        indexed_children: Some(vec![]),
                    });
                };
            }
            None
        }
        _ => None,
    }
}

fn extract_file(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<R>,
    attribute_value: gimli::AttributeValue<R>,
) -> Option<String> {
    match attribute_value {
        gimli::AttributeValue::FileIndex(index) => unit.line_program.as_ref().and_then(|ilnp| {
            let header = ilnp.header();
            header.file(index).and_then(|file_entry| {
                file_entry.directory(header).and_then(|directory| {
                    extract_name(debug_info, directory).and_then(|dir| {
                        extract_name(debug_info, file_entry.path_name())
                            .map(|file| format!("{}/{}", dir, file))
                    })
                })
            })
        }),
        _ => None,
    }
}

fn extract_line(_debug_info: &DebugInfo, attribute_value: gimli::AttributeValue<R>) -> Option<u64> {
    match attribute_value {
        gimli::AttributeValue::Udata(line) => Some(line),
        _ => None,
    }
}

fn extract_name(
    debug_info: &DebugInfo,
    attribute_value: gimli::AttributeValue<R>,
) -> Option<String> {
    match attribute_value {
        gimli::AttributeValue::DebugStrRef(name_ref) => {
            let name_raw = debug_info.dwarf.string(name_ref).unwrap();
            Some(String::from_utf8_lossy(&name_raw).to_string())
        }
        gimli::AttributeValue::String(name) => Some(String::from_utf8_lossy(&name).to_string()),
        _ => None,
    }
}

fn get_piece_value(session: &mut Session, p: &gimli::Piece<DwarfReader>) -> Option<u32> {
    use gimli::Location;

    match &p.location {
        Location::Empty => None,
        Location::Address { address } => Some(*address as u32),
        Location::Value { value } => Some(value.to_u64(0xff_ff_ff_ff).unwrap() as u32),
        Location::Register { register } => {
            let val = session
                .target
                .core
                .read_core_reg(&mut session.probe, (register.0 as u8).into())
                .expect("Failed to read register from target");
            Some(val)
        }
        l => unimplemented!("Location {:?} not implemented", l),
    }
}

pub fn print_all_attributes(
    session: &mut Session,
    frame_base: Option<u32>,
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    tag: &gimli::DebuggingInformationEntry<DwarfReader>,
    print_depth: usize,
) {
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
            }
            Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                loop {
                    use gimli::EvaluationResult::*;

                    result = match result {
                        Complete => break,
                        RequiresMemory { address, size, .. } => {
                            let mut buff = vec![0u8; size as usize];
                            session
                                .probe
                                .read_block8(address as u32, &mut buff)
                                .expect("Failed to read memory");
                            match size {
                                1 => evaluation
                                    .resume_with_memory(gimli::Value::U8(buff[0]))
                                    .unwrap(),
                                2 => {
                                    let val = u16::from(buff[0]) << 8 | u16::from(buff[1]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U16(val))
                                        .unwrap()
                                }
                                4 => {
                                    let val = u32::from(buff[0]) << 24
                                        | u32::from(buff[1]) << 16
                                        | u32::from(buff[2]) << 8
                                        | u32::from(buff[3]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U32(val))
                                        .unwrap()
                                }
                                _ => unimplemented!(),
                            }
                        }
                        RequiresFrameBase => evaluation
                            .resume_with_frame_base(u64::from(frame_base.unwrap()))
                            .unwrap(),
                        x => {
                            println!("{:?}", x);
                            unimplemented!()
                        }
                    }
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            }
            LocationListsRef(_) => {
                println!("LocationList");
            }
            DebugLocListsBase(_) => {
                println!(" LocationList");
            }
            DebugLocListsIndex(_) => {
                println!(" LocationList");
            }
            //_ => println!("{:?}", attr.value()),
            _ => println!("-"),
        }
    }
}
