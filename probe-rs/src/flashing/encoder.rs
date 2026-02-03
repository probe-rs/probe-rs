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
    fn new(flash: FlashLayout, ignore_fills: bool) -> Self {
        let mut compressed_pages = vec![];

        let page_size = flash.pages()[0].data().len();

        let mut compress_image = |image: &[u8], start_addr: u64| {
            if image.is_empty() {
                return;
            }

            use flate2::Compression;
            use flate2::write::ZlibEncoder;

            tracing::debug!("Image length: {} @ {:#010x}", image.len(), start_addr);

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

            tracing::debug!("Compressed length: {}", image_len + 4);

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

        for (page_idx, page) in flash.pages().iter().enumerate() {
            let mut pieces_vec = Vec::new();
            let pieces = if ignore_fills {
                // Remove filled ranges from the data before we compress it.

                // First, collect the relevant fills (the ones that touch the current page).
                let mut relevant_fills = vec![];
                for fill in flash.fills() {
                    if fill.page_index() != page_idx {
                        continue;
                    }

                    relevant_fills.push(fill);
                }

                // Sort them by address, just in case.
                relevant_fills.sort_by_key(|fill| fill.address());

                // Now break the page into pieces, separated by the fills.
                let mut last_offset = 0;
                for fill in relevant_fills {
                    let offset = (fill.address() - page.address()) as usize;

                    let data = &page.data()[last_offset..offset];
                    if !data.is_empty() {
                        pieces_vec.push((page.address() + last_offset as u64, data));
                    }

                    last_offset = offset + fill.size() as usize;
                }

                // Handle the remainder
                let data = &page.data()[last_offset..];
                if !data.is_empty() {
                    pieces_vec.push((page.address() + last_offset as u64, data));
                }

                pieces_vec.as_slice()
            } else {
                &[(page.address(), page.data())]
            };
            for &(address, data) in pieces {
                if address != previous_start_addr + previous_image.len() as u64 {
                    compress_image(&previous_image, previous_start_addr);

                    previous_image.clear();
                    previous_start_addr = address;
                }

                previous_image.extend_from_slice(data);
            }
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

/// Transforms data to be flashed into a format suitable for the flashing algorithm.
pub struct FlashEncoder {
    encoder: Box<dyn EncoderAlgorithm>,
}

impl FlashEncoder {
    /// Creates a new flash encoder with the given flash layout and transfer encoding.
    pub fn new(encoding: TransferEncoding, flash: FlashLayout, ignore_fills: bool) -> Self {
        Self {
            encoder: match encoding {
                TransferEncoding::Raw => Box::new(RawEncoder::new(flash)),
                TransferEncoding::Miniz => Box::new(ZlibEncoder::new(flash, ignore_fills)),
            },
        }
    }

    /// Returns the encoded data.
    pub fn pages(&self) -> &[FlashPage] {
        self.encoder.pages()
    }

    /// Returns the sectors to be erased.
    pub fn sectors(&self) -> &[FlashSector] {
        self.encoder.sectors()
    }

    /// Returns the total size of the encoded data.
    pub fn program_size(&self) -> u64 {
        self.pages().iter().map(|p| p.data().len() as u64).sum()
    }

    /// Returns the final flash layout.
    pub fn flash_layout(&self) -> &FlashLayout {
        self.encoder.layout()
    }
}
