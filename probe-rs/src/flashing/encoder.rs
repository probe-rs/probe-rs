use probe_rs_target::TransferEncoding;

use crate::flashing::{FlashLayout, FlashPage, FlashSector};

trait EncoderAlgorithm {
    fn pages(&self) -> &[FlashPage];
    fn sectors(&self) -> &[FlashSector];
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
}

pub struct FlashEncoder {
    encoder: Box<dyn EncoderAlgorithm>,
}

impl FlashEncoder {
    pub fn new(encoding: TransferEncoding, flash: FlashLayout) -> Self {
        match encoding {
            TransferEncoding::Raw => Self {
                encoder: Box::new(RawEncoder::new(flash)),
            },
        }
    }

    pub fn pages(&self) -> &[FlashPage] {
        self.encoder.pages()
    }

    pub fn sectors(&self) -> &[FlashSector] {
        self.encoder.sectors()
    }
}
