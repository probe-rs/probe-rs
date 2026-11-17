use gimli::{
    BaseAddresses, CieOrFde, DebugFrame, DebugFrameOffset, FrameDescriptionEntry,
    PartialFrameDescriptionEntry, UnwindSection,
};

use crate::debug_info::DwarfReader;

/// The FDEs (Frame Description Entries) of a `.debug_frame` section, sorted by start address.
#[derive(Default)]
pub(crate) struct FdeIndex {
    entries: Vec<FdeEntry>,
    /// `max_end[i]` is the largest end address in `entries[..=i]`.
    max_end: Vec<u64>,
}

struct FdeEntry {
    start: u64,
    end: u64,
    offset: usize,
}

impl FdeEntry {
    /// The entry for `partial`, or `None` if it does not parse or covers no address.
    fn parse(
        partial: &PartialFrameDescriptionEntry<'_, DebugFrame<DwarfReader>, DwarfReader>,
    ) -> Option<Self> {
        let fde = partial
            .parse(DebugFrame::cie_from_offset)
            .inspect_err(|error| {
                tracing::warn!(
                    "UNWIND: Skipped the FDE at .debug_frame offset {:#x}: {error}",
                    partial.offset()
                )
            })
            .ok()?;

        (fde.initial_address() < fde.end_address()).then(|| FdeEntry {
            start: fde.initial_address(),
            end: fde.end_address(),
            offset: fde.offset(),
        })
    }
}

impl FdeIndex {
    pub(crate) fn new(frame_section: &DebugFrame<DwarfReader>) -> Self {
        let bases = BaseAddresses::default();
        let mut entries = Vec::new();
        let mut iter = frame_section.entries(&bases);
        loop {
            let partial = match iter.next() {
                Ok(Some(CieOrFde::Fde(partial))) => partial,
                Ok(Some(CieOrFde::Cie(_))) => continue,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!("UNWIND: Stopped reading .debug_frame: {error}");
                    break;
                }
            };
            entries.extend(FdeEntry::parse(&partial));
        }

        // The sort is stable, so entries with the same start stay in section order.
        entries.sort_by_key(|entry| entry.start);
        let max_end = entries
            .iter()
            .scan(0, |max_end, entry| {
                *max_end = entry.end.max(*max_end);
                Some(*max_end)
            })
            .collect();

        Self { entries, max_end }
    }

    /// The FDE that covers `address`, from `frame_section`, the section this index was built from.
    pub(crate) fn fde_for_address(
        &self,
        frame_section: &DebugFrame<DwarfReader>,
        address: u64,
    ) -> gimli::Result<FrameDescriptionEntry<DwarfReader>> {
        // FDEs can overlap: the linker places the FDEs of the functions that it removed at
        // address 0. Of the FDEs that cover the address, the first in the section wins, as in a
        // linear search of the section.
        let candidates = self.entries.partition_point(|entry| entry.start <= address);
        let offset = self.entries[..candidates]
            .iter()
            .zip(&self.max_end[..candidates])
            .rev()
            .take_while(|(_, max_end)| **max_end > address)
            .filter(|(entry, _)| address < entry.end)
            .map(|(entry, _)| entry.offset)
            .min()
            .ok_or(gimli::Error::NoUnwindInfoForAddress)?;

        frame_section.fde_from_offset(
            &BaseAddresses::default(),
            DebugFrameOffset(offset),
            DebugFrame::cie_from_offset,
        )
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use gimli::{
        BaseAddresses, DebugFrame, EndianArcSlice, Format, RunTimeEndian, UnwindSection,
        write::{Address, CommonInformationEntry, EndianVec, FrameDescriptionEntry, FrameTable},
    };

    use super::FdeIndex;
    use crate::debug_info::DwarfReader;

    /// A `.debug_frame` with one FDE for each `(start, length)`, in that order.
    fn frame_section(fdes: &[(u64, u32)]) -> DebugFrame<DwarfReader> {
        let encoding = gimli::Encoding {
            format: Format::Dwarf32,
            version: 1,
            address_size: 4,
        };
        let mut table = FrameTable::default();
        let cie = table.add_cie(CommonInformationEntry::new(
            encoding,
            1,
            -4,
            gimli::Register(14),
        ));
        for &(start, length) in fdes {
            table.add_fde(
                cie,
                FrameDescriptionEntry::new(Address::Constant(start), length),
            );
        }
        let mut section = gimli::write::DebugFrame(EndianVec::new(RunTimeEndian::Little));
        table.write_debug_frame(&mut section).unwrap();

        let data: Arc<[u8]> = Arc::from(section.0.into_vec());
        let mut section = DebugFrame::from(EndianArcSlice::new(data, RunTimeEndian::Little));
        section.set_address_size(4);
        section
    }

    /// The `(start, length)` of the FDE that the index finds for `address`.
    fn find(fdes: &[(u64, u32)], address: u64) -> Option<(u64, u64)> {
        let section = frame_section(fdes);
        let found = FdeIndex::new(&section)
            .fde_for_address(&section, address)
            .ok();

        let linear = section
            .fde_for_address(
                &BaseAddresses::default(),
                address,
                DebugFrame::cie_from_offset,
            )
            .ok();
        assert_eq!(found, linear, "differs from a linear search");

        found.map(|fde| (fde.initial_address(), fde.len()))
    }

    #[test]
    fn finds_the_fde_that_covers_the_address() {
        let fdes = [(0x300, 0x10), (0x100, 0x10), (0x200, 0x10)];
        assert_eq!(find(&fdes, 0x100), Some((0x100, 0x10)));
        assert_eq!(find(&fdes, 0x20f), Some((0x200, 0x10)));
        assert_eq!(find(&fdes, 0x305), Some((0x300, 0x10)));
    }

    #[test]
    fn finds_nothing_between_fdes() {
        let fdes = [(0x100, 0x10), (0x200, 0x10)];
        assert_eq!(find(&fdes, 0xff), None);
        assert_eq!(find(&fdes, 0x110), None);
        assert_eq!(find(&fdes, 0x210), None);
    }

    #[test]
    fn overlapping_fdes_resolve_to_the_first_in_the_section() {
        let fdes = [(0x200, 0x10), (0x0, 0x1000), (0x100, 0x10), (0x0, 0x20)];
        assert_eq!(find(&fdes, 0x10), Some((0x0, 0x1000)));
        assert_eq!(find(&fdes, 0x105), Some((0x0, 0x1000)));
        assert_eq!(find(&fdes, 0x205), Some((0x200, 0x10)));
        assert_eq!(find(&fdes, 0x800), Some((0x0, 0x1000)));
        assert_eq!(find(&fdes, 0x1000), None);
    }

    #[test]
    fn a_later_long_fde_covers_an_address_past_shorter_ones() {
        let fdes = [(0x100, 0x10), (0x0, 0x20), (0x0, 0x1000)];
        assert_eq!(find(&fdes, 0x10), Some((0x0, 0x20)));
        assert_eq!(find(&fdes, 0x80), Some((0x0, 0x1000)));
        assert_eq!(find(&fdes, 0x200), Some((0x0, 0x1000)));
    }

    #[test]
    fn ignores_empty_fdes() {
        let fdes = [(0x100, 0), (0x100, 0x10)];
        assert_eq!(find(&fdes, 0x100), Some((0x100, 0x10)));
    }
}
