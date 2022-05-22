use svg::{
    node::element::{Group, Rectangle, Text},
    node::Text as Content,
    Document, Node,
};

use super::*;

/// A structure which can be used to visualize the built contents of a flash.
pub struct FlashVisualizer<'layout> {
    flash_layout: &'layout FlashLayout,
}

impl<'layout> FlashVisualizer<'layout> {
    pub(super) fn new(flash_layout: &'layout FlashLayout) -> Self {
        Self { flash_layout }
    }

    /// Calculates the position in a [0, 100] range
    /// depending on the given address and the highest known sector end address.
    fn memory_to_local(&self, address: u64) -> f32 {
        let top_sector_address = self
            .flash_layout
            .sectors()
            .last()
            .map_or(0, |s| s.address() + s.size());

        address as f32 / top_sector_address as f32 * 100.0
    }

    fn memory_block(&self, address: u64, size: u64, dimensions: (u32, u32)) -> Group {
        let height = self.memory_to_local(size);
        let start = 100.0 - self.memory_to_local(address) - height;

        let mut group = Group::new();

        group.append(
            Rectangle::new()
                .set("x", dimensions.0)
                .set("y", start)
                .set("width", dimensions.1)
                .set("height", height),
        );

        group.append(
            Text::new()
                .set("x", dimensions.0 + 1)
                .set("y", start + height - 2.0)
                .set("font-size", 5)
                .set("font-family", "Arial")
                .set("fill", "Black")
                .add(Content::new(format!("{:#08X?}", address))),
        );

        group.append(
            Text::new()
                .set("x", dimensions.0 + 1)
                .set("y", start + 5.0)
                .set("font-size", 5)
                .set("font-family", "Arial")
                .set("fill", "Black")
                .add(Content::new(format!("{:#08X?}", address + size))),
        );

        group
    }

    /// Generates an SVG in string form which visualizes the given flash contents.
    ///
    /// This generator was introduced to debug the library flashing algorithms
    /// but can also be used to track what contents of flash will be erased and written.
    pub fn generate_svg(&self) -> String {
        let mut document = Document::new();
        let mut group = Group::new().set("transform", "scale(1, 1)");

        for sector in self.flash_layout.sectors() {
            let rectangle = self
                .memory_block(sector.address(), sector.size(), (50, 50))
                .set("fill", "CornflowerBlue");

            group.append(rectangle);
        }

        for page in self.flash_layout.pages() {
            let rectangle = self
                .memory_block(page.address(), page.size() as u64, (100, 50))
                .set("fill", "Crimson");
            // .set("stroke", "Black")
            // .set("stroke-width", 1);
            group.append(rectangle);
        }

        for block in self.flash_layout.data_blocks() {
            let rectangle = self
                .memory_block(block.address(), block.size(), (150, 50))
                .set("fill", "MediumSeaGreen");

            group.append(rectangle);
        }

        for fill in self.flash_layout.fills() {
            let rectangle = self
                .memory_block(fill.address(), fill.size(), (150, 50))
                .set("fill", "SandyBrown");

            group.append(rectangle);
        }

        document.append(group);
        document.assign("viewBox", (0, -20, 300, 140));

        format!("{}", document)
    }

    /// Generates an SVG which visualizes the given flash contents
    /// and writes the SVG into the file at the given `path`.
    ///
    /// This is aequivalent to [FlashVisualizer::generate_svg] with the difference of operating on a file instead of a string.
    pub fn write_svg(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        use std::fs::OpenOptions;
        use std::io::Write;

        let svg = self.generate_svg();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.as_ref())?;

        file.write_all(svg.as_bytes())
    }
}
