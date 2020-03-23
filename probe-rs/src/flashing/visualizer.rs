use svg::{
    node::element::{Group, Rectangle, Text},
    node::Text as Content,
    Document, Node,
};

use super::*;

pub struct FlashVisualizer<'a> {
    flash_layout: &'a FlashLayout,
}

impl<'a> FlashVisualizer<'a> {
    pub(super) fn new(flash_layout: &'a FlashLayout) -> Self {
        Self { flash_layout }
    }

    /// Calculates the position in a [0, 100] range
    /// depending on the given address and the highest known sector end address.
    fn memory_to_local(&self, address: u32) -> f32 {
        let top_sector_address = self
            .flash_layout
            .sectors()
            .last()
            .map_or(0, |s| s.address() + s.size());

        address as f32 / top_sector_address as f32 * 100.0
    }

    fn memory_block(&self, address: u32, size: u32, dimensions: (u32, u32)) -> Group {
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
                .memory_block(page.address(), page.size(), (100, 50))
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

    pub fn write_svg(&self, name: impl AsRef<str>) -> std::io::Result<()> {
        use std::fs::OpenOptions;
        use std::io::Write;

        let svg = self.generate_svg();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(name.as_ref())?;

        file.write_all(svg.as_bytes())
    }
}
