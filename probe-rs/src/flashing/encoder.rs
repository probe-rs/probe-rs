use std::io::Write as _;

use probe_rs_target::TransferEncoding;

use crate::flashing::{FlashLayout, FlashPage, FlashSector};

trait EncoderAlgorithm {
    fn pages(&self) -> &[FlashPage];
    fn sectors(&self) -> &[FlashSector];
    fn layout(&self) -> &FlashLayout;
}

/// No-op encoder.
struct RawEncoder {
    flash: FlashLayout,
}

impl RawEncoder {
    fn new(flash: FlashLayout) -> Self {
        Self { flash }
    }
}

impl EncoderAlgorithm for RawEncoder {
    fn pages(&self) -> &[FlashPage] {
        self.flash.pages()
    }

    fn sectors(&self) -> &[FlashSector] {
        self.flash.sectors()
    }

    fn layout(&self) -> &FlashLayout {
        &self.flash
    }
}

/// Miniz-encoder.
///
/// The encoder will break up the flash contents into contiguous images, compress each of them
/// separately and it will output flash pages with the *start address* of the contiguous image.
///
/// The flash loader that accepts this format must be able to track the offset in the current image.
/// The end of an image is signaled by the first non-full page. This may include an empty page.
struct ZlibEncoder {
    flash: FlashLayout,
    compressed_pages: Vec<FlashPage>,
}

impl ZlibEncoder {
    fn new(flash: FlashLayout) -> Self {
        let mut compressed_pages = vec![];

        let page_size = flash.pages()[0].data().len();

        let mut compress_image = |image: &[u8], start_addr: u64| {
            if image.is_empty() {
                return;
            }

            use flate2::write::ZlibEncoder;
            use flate2::Compression;

            // This page is not contiguous with the previous one, finish the previous image.
            let mut e = ZlibEncoder::new(Vec::new(), Compression::best());
            // These unwraps are okay because we are writing to a Vec and that is infallible.
            e.write_all(image).unwrap();
            let compressed = e.finish().unwrap();

            let image_len = compressed.len();
            // We chunk up the image and prepend the compressed image's length to the first chunk.
            let first_chunk_len = image_len.min(page_size - 4);
            let (first, rest) = compressed.split_at(first_chunk_len);

            let first_chunk = (image_len as u32)
                .to_le_bytes()
                .into_iter()
                .chain(first.iter().copied())
                .collect::<Vec<u8>>();

            // We add each page with `start_addr`. The address identifies the image and the
            // flash loader is responsible for tracking the write offset in the current image.
            compressed_pages.push(FlashPage {
                address: start_addr,
                data: first_chunk,
            });

            for chunk in rest.chunks(page_size) {
                compressed_pages.push(FlashPage {
                    address: start_addr,
                    data: chunk.to_vec(),
                });
            }
        };

        // (start_addr, compressed_image)
        let mut previous_image = vec![];
        let mut previous_start_addr = 0;

        for page in flash.pages() {
            if page.address() != previous_start_addr + previous_image.len() as u64 {
                compress_image(&previous_image, previous_start_addr);

                previous_image.clear();
                previous_start_addr = page.address();
            }

            previous_image.extend_from_slice(page.data());
        }

        compress_image(&previous_image, previous_start_addr);

        tracing::debug!(
            "Compressed/original: {}/{}",
            compressed_pages.iter().map(|p| p.size()).sum::<u32>(),
            flash.pages().iter().map(|p| p.size()).sum::<u32>()
        );

        Self {
            compressed_pages,
            flash,
        }
    }
}

impl EncoderAlgorithm for ZlibEncoder {
    fn pages(&self) -> &[FlashPage] {
        &self.compressed_pages
    }

    fn sectors(&self) -> &[FlashSector] {
        self.flash.sectors()
    }

    fn layout(&self) -> &FlashLayout {
        &self.flash
    }
}

pub struct FlashEncoder {
    encoder: Box<dyn EncoderAlgorithm>,
}

impl FlashEncoder {
    pub fn new(encoding: TransferEncoding, flash: FlashLayout) -> Self {
        Self {
            encoder: match encoding {
                TransferEncoding::Raw => Box::new(RawEncoder::new(flash)),
                TransferEncoding::Miniz => Box::new(ZlibEncoder::new(flash)),
            },
        }
    }

    pub fn pages(&self) -> &[FlashPage] {
        self.encoder.pages()
    }

    pub fn sectors(&self) -> &[FlashSector] {
        self.encoder.sectors()
    }

    pub fn program_size(&self) -> u64 {
        self.pages().iter().map(|p| p.data().len() as u64).sum()
    }

    pub fn flash_layout(&self) -> &FlashLayout {
        self.encoder.layout()
    }
}
