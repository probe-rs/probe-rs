use core::{iter::FromIterator, ops::Range};
use std::{
    borrow::Cow,
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure};
use gimli::{read::Reader, DebuggingInformationEntry, Dwarf, Unit};
use intervaltree::{Element, IntervalTree};
use object::{Object as _, ObjectSection as _};

pub type Map = IntervalTree<u64, Frame>;

pub fn from(object: &object::File, live_functions: &HashSet<&str>) -> Result<Map, anyhow::Error> {
    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    let load_section = |id: gimli::SectionId| {
        Ok(if let Some(s) = object.section_by_name(id.name()) {
            s.uncompressed_data().unwrap_or(Cow::Borrowed(&[][..]))
        } else {
            Cow::Borrowed(&[][..])
        })
    };
    let load_section_sup = |_| Ok(Cow::Borrowed(&[][..]));

    let dwarf_cow =
        gimli::Dwarf::<Cow<[u8]>>::load::<_, _, anyhow::Error>(&load_section, &load_section_sup)?;

    let borrow_section: &dyn for<'a> Fn(
        &'a Cow<[u8]>,
    ) -> gimli::EndianSlice<'a, gimli::RunTimeEndian> =
        &|section| gimli::EndianSlice::new(&*section, endian);

    let dwarf = dwarf_cow.borrow(&borrow_section);

    let mut units = dwarf.debug_info.units();

    let mut elements = vec![];
    while let Some(header) = units.next()? {
        let unit = dwarf.unit(header)?;
        let abbrev = header.abbreviations(&dwarf.debug_abbrev)?;

        let mut cursor = header.entries(&abbrev);

        ensure!(cursor.next_dfs()?.is_some(), "empty DWARF?");

        let mut depth = 0;
        // None = outside a subprogram DIE
        // Some(depth) = inside a subprogram DIE
        let mut subprogram_depth = None;
        while let Some((delta_depth, entry)) = cursor.next_dfs()? {
            depth += delta_depth;

            if let Some(subprogram_depth_val) = subprogram_depth {
                if depth <= subprogram_depth_val {
                    // leaving subprogram DIE
                    subprogram_depth = None;
                }
            }

            if entry.tag() == gimli::constants::DW_TAG_subprogram {
                if let Some(sub) = Subprogram::from_die(entry, depth, &dwarf)? {
                    if let Span::Pc(range) = sub.span.clone() {
                        if live_functions.contains(&*sub.name) {
                            // sanity check: nested subprograms have never been observed in practice
                            assert!(subprogram_depth.is_none(), "BUG? nested subprogram");

                            subprogram_depth = Some(depth);
                            let name = demangle(&sub.name);
                            elements.push(Element {
                                range,
                                value: Frame {
                                    name,
                                    depth,
                                    call_loc: None,
                                    decl_loc: Location {
                                        file: file_index_to_path(sub.decl_file, &unit, &dwarf)?,
                                        line: sub.decl_line,
                                    },
                                },
                            });
                        } else {
                            // we won't walk into subprograms that are were GC-ed by the linker
                        }
                    } else {
                        // subprograms with "inlined" span will be referred to by the 'origin'
                        // field of `InlinedSubroutine`s so we don't add them to the list at this
                        // point. Also, they don't have PC span info and won't appear as a symbol
                        // in the .symtab
                    }
                }
            } else if subprogram_depth.is_some() {
                // within a 'live' subroutine (subroutine was not GC-ed by the linker)
                if entry.tag() == gimli::constants::DW_TAG_inlined_subroutine {
                    let inline_sub = InlinedSubroutine::from_die(entry, depth, &dwarf, &unit)?;
                    elements.push(Element {
                        range: inline_sub.pc,
                        value: Frame {
                            name: demangle(&inline_sub.origin.name),
                            depth,
                            call_loc: Some(Location {
                                file: file_index_to_path(inline_sub.call_file, &unit, &dwarf)?,
                                line: inline_sub.call_line,
                            }),
                            decl_loc: Location {
                                file: file_index_to_path(
                                    inline_sub.origin.decl_file,
                                    &unit,
                                    &dwarf,
                                )?,
                                line: inline_sub.origin.decl_line,
                            },
                        },
                    })
                } else if entry.tag() == gimli::constants::DW_TAG_lexical_block
                    || entry.tag() == gimli::constants::DW_TAG_variable
                {
                    // TODO extract more fine grained (statement-level) location information
                }
            }
        }
    }

    Ok(IntervalTree::from_iter(elements))
}

#[derive(Debug)]
pub struct Frame {
    // unmangled function name
    pub name: String,
    // depth in the DIE tree
    pub depth: isize,
    pub call_loc: Option<Location>,
    pub decl_loc: Location,
}

#[derive(Debug)]
pub struct Location {
    pub file: PathBuf,
    pub line: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum Span {
    Pc(Range<u64>),
    Inlined,
}

#[derive(Debug)]
struct Subprogram {
    // depth in the DIE tree
    depth: isize,
    name: String,
    span: Span,
    decl_file: u64,
    decl_line: u64,
}

impl Subprogram {
    /// returns `None` if `entry` has no "name"
    fn from_die<R>(
        entry: &DebuggingInformationEntry<R>,
        depth: isize,
        dwarf: &Dwarf<R>,
    ) -> Result<Option<Self>, anyhow::Error>
    where
        R: Reader,
    {
        assert_eq!(entry.tag(), gimli::constants::DW_TAG_subprogram);

        let mut attrs = entry.attrs();

        let mut inlined = false;
        let mut linkage_name = None;
        let mut low_pc = None;
        let mut name = None;
        let mut pc_offset = None;
        let mut decl_file = None;
        let mut decl_line = None;
        while let Some(attr) = attrs.next()? {
            match attr.name() {
                gimli::constants::DW_AT_low_pc => {
                    if let gimli::AttributeValue::Addr(addr) = attr.value() {
                        low_pc = Some(addr);
                    } else {
                        unreachable!()
                    }
                }

                gimli::constants::DW_AT_high_pc => {
                    pc_offset = Some(attr.value().udata_value().expect("unreachable"));
                }

                gimli::constants::DW_AT_linkage_name => {
                    if let gimli::AttributeValue::DebugStrRef(off) = attr.value() {
                        linkage_name = Some(off);
                    } else {
                        unreachable!()
                    }
                }

                gimli::constants::DW_AT_name => {
                    if let gimli::AttributeValue::DebugStrRef(off) = attr.value() {
                        name = Some(off);
                    } else {
                        unreachable!()
                    }
                }

                gimli::constants::DW_AT_inline => {
                    if let gimli::AttributeValue::Inline(gimli::constants::DW_INL_inlined) =
                        attr.value()
                    {
                        inlined = true;
                    }
                }

                gimli::constants::DW_AT_decl_file => {
                    if let gimli::AttributeValue::FileIndex(idx) = attr.value() {
                        decl_file = Some(idx);
                    }
                }

                gimli::constants::DW_AT_decl_line => {
                    if let gimli::AttributeValue::Udata(line) = attr.value() {
                        decl_line = Some(line);
                    }
                }

                _ => {}
            }
        }

        if let Some(off) = linkage_name.or(name) {
            let name = dwarf.string(off)?.to_string()?.into_owned();
            let decl_file = decl_file.expect("no `decl_file`");
            let decl_line = decl_line.expect("no `decl_line`");

            Ok(Some(Subprogram {
                depth,
                span: if inlined {
                    Span::Inlined
                } else {
                    let low_pc = low_pc.expect("no `low_pc`");
                    let pc_off = pc_offset.expect("no `high_pc`");
                    Span::Pc(low_pc..(low_pc + pc_off))
                },
                name,
                decl_file,
                decl_line,
            }))
        } else {
            // TODO what are these nameless subroutines? They seem to have "abstract origin" info
            Ok(None)
        }
    }
}

#[derive(Debug)]
struct InlinedSubroutine {
    call_file: u64,
    call_line: u64,
    origin: Subprogram,
    pc: Range<u64>,
}

impl InlinedSubroutine {
    fn from_die<R>(
        entry: &DebuggingInformationEntry<R>,
        depth: isize,
        dwarf: &Dwarf<R>,
        unit: &Unit<R>,
    ) -> Result<Self, anyhow::Error>
    where
        R: Reader,
    {
        assert_eq!(entry.tag(), gimli::constants::DW_TAG_inlined_subroutine);

        let mut attrs = entry.attrs();

        let mut at_range = None;
        let mut call_file = None;
        let mut call_line = None;
        let mut low_pc = None;
        let mut origin = None;
        let mut pc_offset = None;
        while let Some(attr) = attrs.next()? {
            match attr.name() {
                gimli::constants::DW_AT_abstract_origin => {
                    if let gimli::AttributeValue::UnitRef(off) = attr.value() {
                        let other_entry = unit.entry(off)?;

                        let sub = Subprogram::from_die(&other_entry, depth, dwarf)?.unwrap();
                        origin = Some(sub);
                    } else {
                        unreachable!()
                    }
                }

                gimli::constants::DW_AT_ranges => {
                    if let gimli::AttributeValue::RangeListsRef(off) = attr.value() {
                        let r = dwarf
                            .ranges(&unit, off)?
                            .next()?
                            .expect("unexpected end of range list");
                        at_range = Some(r.begin..r.end);
                    }
                }

                gimli::constants::DW_AT_low_pc => {
                    if let gimli::AttributeValue::Addr(addr) = attr.value() {
                        low_pc = Some(addr);
                    } else {
                        unreachable!()
                    }
                }

                gimli::constants::DW_AT_high_pc => {
                    pc_offset = Some(attr.value().udata_value().expect("unreachable"));
                }

                gimli::constants::DW_AT_call_file => {
                    if let gimli::AttributeValue::FileIndex(idx) = attr.value() {
                        call_file = Some(idx);
                    }
                }

                gimli::constants::DW_AT_call_line => {
                    if let gimli::AttributeValue::Udata(line) = attr.value() {
                        call_line = Some(line);
                    }
                }

                _ => {}
            }
        }

        let pc = at_range.unwrap_or_else(|| {
            let start = low_pc.expect("no low_pc");
            let off = pc_offset.expect("no high_pc");
            start..start + off
        });

        Ok(InlinedSubroutine {
            origin: origin.expect("no abstract_origin"),
            call_file: call_file.expect("no call_file"),
            call_line: call_line.expect("no call_line"),
            pc,
        })
    }
}

fn demangle(function: &str) -> String {
    let mut demangled = rustc_demangle::demangle(function).to_string();
    // remove trailing hash (`::he40fe02240f4a81d`)
    // strip the hash (e.g. `::hd881d91ced85c2b0`)
    let hash_len = "::hd881d91ced85c2b0".len();
    if let Some(pos) = demangled.len().checked_sub(hash_len) {
        let maybe_hash = &demangled[pos..];
        if maybe_hash.starts_with("::h") {
            for _ in 0..hash_len {
                demangled.pop();
            }
        }
    }

    demangled
}

// XXX copy-pasted from defmt/elf2table :sadface:
fn file_index_to_path<R>(
    index: u64,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<PathBuf, anyhow::Error>
where
    R: gimli::read::Reader,
{
    ensure!(index != 0, "`FileIndex` was zero");

    let header = if let Some(program) = &unit.line_program {
        program.header()
    } else {
        bail!("no `LineProgram`");
    };

    let file = if let Some(file) = header.file(index) {
        file
    } else {
        bail!("no `FileEntry` for index {}", index)
    };

    let mut p = PathBuf::new();
    if let Some(dir) = file.directory(header) {
        let dir = dwarf.attr_string(unit, dir)?;
        let dir_s = dir.to_string_lossy()?;
        let dir = Path::new(&dir_s[..]);

        if !dir.is_absolute() {
            if let Some(ref comp_dir) = unit.comp_dir {
                p.push(&comp_dir.to_string_lossy()?[..]);
            }
        }
        p.push(&dir);
    }

    p.push(
        &dwarf
            .attr_string(unit, file.path_name())?
            .to_string_lossy()?[..],
    );

    Ok(p)
}
