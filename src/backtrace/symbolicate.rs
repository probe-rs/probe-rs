//! Turns PC addresses into function names and locations

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
};

use addr2line::fallible_iterator::FallibleIterator as _;
use either::Either;
use gimli::{EndianReader, RunTimeEndian};
use object::{Object as _, SymbolMap, SymbolMapName};

use crate::{cortexm, elf::ProcessedElf};

use super::unwind::RawFrame;

pub(crate) fn frames(
    raw_frames: &[RawFrame],
    current_dir: &Path,
    elf: &ProcessedElf,
) -> Vec<Frame> {
    let mut frames = vec![];

    let symtab = elf.symbol_map();
    let addr2line = addr2line::Context::new(&**elf).ok();

    for raw_frame in raw_frames {
        match raw_frame {
            RawFrame::Exception => frames.push(Frame::Exception),

            RawFrame::Subroutine { pc } => {
                for subroutine in Subroutine::from_pc(
                    *pc,
                    addr2line.as_ref(),
                    &elf.live_functions,
                    current_dir,
                    &symtab,
                ) {
                    frames.push(Frame::Subroutine(subroutine))
                }
            }
        }
    }

    frames
}

/// Processed frame
#[derive(Debug)]
pub(crate) enum Frame {
    Exception,
    Subroutine(Subroutine),
}

/// "Symbolicated" and de-inlined subroutine frame
#[derive(Debug)]
pub(crate) struct Subroutine {
    pub(crate) name_or_pc: Either<String, u32>,
    pub(crate) location: Option<Location>,
}

type A2lContext = addr2line::Context<EndianReader<RunTimeEndian, Rc<[u8]>>>;

impl Subroutine {
    fn from_pc(
        pc: u32,
        addr2line: Option<&A2lContext>,
        live_functions: &HashSet<&str>,
        current_dir: &Path,
        symtab: &SymbolMap<SymbolMapName>,
    ) -> Vec<Subroutine> {
        addr2line
            .and_then(|addr2line| {
                Self::from_debuginfo(pc, addr2line, live_functions, current_dir, symtab)
            })
            .unwrap_or_else(|| vec![Self::from_symtab(pc, symtab)])
    }

    fn from_debuginfo(
        pc: u32,
        addr2line: &A2lContext,
        live_functions: &HashSet<&str>,
        current_dir: &Path,
        symtab: &SymbolMap<SymbolMapName>,
    ) -> Option<Vec<Subroutine>> {
        let frames: Vec<_> = addr2line.find_frames(pc as u64).ok()?.collect().ok()?;

        let top_subroutine = frames.last();

        let has_valid_debuginfo = if let Some(function) =
            top_subroutine.and_then(|subroutine| subroutine.function.as_ref())
        {
            live_functions.contains(&*function.raw_name().ok()?)
        } else {
            false
        };

        if !has_valid_debuginfo {
            return None;
        }

        let mut subroutines = vec![];

        for frame in &frames {
            let demangled_name = frame
                .function
                .as_ref()
                .and_then(|function| function.demangle().ok())
                .map(|cow| cow.into_owned());

            // XXX if there was inlining AND there's no function name info we'll report several
            // frames with the same PC
            let name_or_pc = demangled_name
                .map(Either::Left)
                .unwrap_or_else(|| name_from_symtab(pc, symtab));

            let location = if let Some((file, line, column)) =
                frame.location.as_ref().and_then(|loc| {
                    loc.file
                        .and_then(|file| loc.line.map(|line| (file, line, loc.column)))
                }) {
                let fullpath = Path::new(file);
                let (path, is_local) = if let Ok(relpath) = fullpath.strip_prefix(&current_dir) {
                    (relpath, true)
                } else {
                    (fullpath, false)
                };

                Some(Location {
                    column,
                    path_is_relative: is_local,
                    line,
                    path: path.to_owned(),
                })
            } else {
                None
            };

            subroutines.push(Subroutine {
                name_or_pc,
                location,
            })
        }

        Some(subroutines)
    }

    fn from_symtab(pc: u32, symtab: &SymbolMap<SymbolMapName>) -> Subroutine {
        Subroutine {
            name_or_pc: name_from_symtab(pc, symtab),
            location: None,
        }
    }
}

fn name_from_symtab(pc: u32, symtab: &SymbolMap<SymbolMapName>) -> Either<String, u32> {
    // the .symtab appears to use address ranges that have their thumb bits set (e.g.
    // `0x101..0x200`). Passing the `pc` with the thumb bit cleared (e.g. `0x100`) to the
    // lookup function sometimes returns the *previous* symbol. Work around the issue by
    // setting `pc`'s thumb bit before looking it up
    let address = cortexm::set_thumb_bit(pc) as u64;

    symtab
        .get(address)
        .map(|symbol| symbol.name().to_owned())
        .map(Either::Left)
        .unwrap_or(Either::Right(pc))
}

#[derive(Debug)]
pub(crate) struct Location {
    pub(crate) column: Option<u32>,
    pub(crate) path_is_relative: bool,
    pub(crate) line: u32,
    pub(crate) path: PathBuf,
}
