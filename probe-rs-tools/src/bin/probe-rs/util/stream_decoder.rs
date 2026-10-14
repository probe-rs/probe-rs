use anyhow::{Context, anyhow};
use defmt_decoder::{
    DecodeError, StreamDecoder as DefmtStreamDecoder,
    log::format::{Formatter, FormatterConfig, FormatterFormat},
};
use time::{OffsetDateTime, UtcOffset, macros::format_description};

use std::{
    fmt::{self, Display, Write},
    sync::Arc,
};

use probe_rs_rpc::rtt_config::DataFormat;

use probe_rs_zephyr::log::Decoder as ZephyrLogDecoder;

/// Decodes a byte stream from the target, for example an RTT channel.
pub enum StreamDecoder {
    String(TextDecoder),
    BinaryLE,
    Defmt {
        processor: DefmtProcessor,
    },
    /// Zephyr dictionary-based logging.
    ZephyrDict {
        processor: ZephyrLogDecoder,
    },
    /// The format is detected from the data.
    Auto {
        decoder: AutoDecoder,
    },
}

impl From<&StreamDecoder> for DataFormat {
    fn from(config: &StreamDecoder) -> Self {
        match config {
            StreamDecoder::String(_) => DataFormat::String,
            StreamDecoder::BinaryLE => DataFormat::BinaryLE,
            StreamDecoder::Defmt { .. } => DataFormat::Defmt,
            // The decoded output is text.
            StreamDecoder::ZephyrDict { .. } => DataFormat::String,
            StreamDecoder::Auto { .. } => DataFormat::String,
        }
    }
}

impl fmt::Debug for StreamDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamDecoder::String(decoder) => f.debug_tuple("String").field(decoder).finish(),
            StreamDecoder::BinaryLE => f.debug_struct("BinaryLE").finish(),
            StreamDecoder::Defmt { .. } => f.debug_struct("Defmt").finish_non_exhaustive(),
            StreamDecoder::ZephyrDict { .. } => {
                f.debug_struct("ZephyrDict").finish_non_exhaustive()
            }
            StreamDecoder::Auto { decoder } => f
                .debug_struct("Auto")
                .field("format", &decoder.format)
                .finish_non_exhaustive(),
        }
    }
}

impl StreamDecoder {
    /// Returns whether the channel is expected to output binary data (`true`)
    /// or human-readable strings (`false`).
    pub fn is_binary(&self) -> bool {
        matches!(self, StreamDecoder::BinaryLE)
    }

    pub fn process<'d>(&mut self, buffer: &'d [u8]) -> anyhow::Result<Option<DecodedData<'d>>> {
        // Prevent the format processors generating empty strings.
        if buffer.is_empty() {
            return Ok(None);
        }

        let string = match self {
            StreamDecoder::BinaryLE => return Ok(Some(DecodedData::Binary(buffer))),
            StreamDecoder::String(decoder) => decoder.process(buffer),
            StreamDecoder::Defmt { processor } => processor.process(buffer)?,
            StreamDecoder::ZephyrDict { processor } => processor.process(buffer),
            StreamDecoder::Auto { decoder } => decoder.process(buffer)?,
        };

        if string.is_empty() {
            return Ok(None);
        }
        Ok(Some(DecodedData::String(string)))
    }
}

/// Decodes text. Adds a timestamp at the start of each line, if enabled.
///
/// Keeps an UTF-8 character that the end of a chunk splits until the next chunk.
#[derive(Debug)]
pub struct TextDecoder {
    /// UTC offset used for creating timestamps, if enabled.
    ///
    /// Getting the offset can fail in multi-threaded programs,
    /// so it needs to be stored.
    timestamp_offset: Option<UtcOffset>,
    show_timestamps: bool,
    at_line_start: bool,
    /// The start of an UTF-8 character that the next chunk completes.
    partial: Vec<u8>,
}

impl TextDecoder {
    pub fn new(timestamp_offset: Option<UtcOffset>, show_timestamps: bool) -> Self {
        Self {
            timestamp_offset,
            show_timestamps,
            at_line_start: true,
            partial: Vec::new(),
        }
    }

    pub fn process(&mut self, data: &[u8]) -> String {
        self.partial.extend_from_slice(data);
        let complete = self.partial.len() - incomplete_utf8_tail(&self.partial);
        let tail = self.partial.split_off(complete);
        let incoming = String::from_utf8_lossy(&self.partial).into_owned();
        self.partial = tail;

        let timestamp = self
            .timestamp_offset
            .filter(|_| self.show_timestamps)
            .map(|offset| {
                OffsetDateTime::now_utc()
                    .to_offset(offset)
                    .format(format_description!(
                        "[hour repr:24]:[minute]:[second].[subsecond digits:3]"
                    ))
                    .expect("Incorrect format string. This shouldn't happen.")
            });

        let mut output = String::new();
        for line in incoming.split_inclusive('\n') {
            if self.at_line_start
                && let Some(timestamp) = &timestamp
            {
                write!(output, "{timestamp}: ").expect("Writing to String cannot fail");
            }
            output.push_str(line);
            self.at_line_start = line.ends_with('\n');
        }
        output
    }
}

/// Returns the length of an incomplete UTF-8 character at the end of `data`.
fn incomplete_utf8_tail(data: &[u8]) -> usize {
    for len in 1..=data.len().min(3) {
        let char_len = match data[data.len() - len] {
            // A continuation byte: look further back for the first byte.
            0x80..=0xBF => continue,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return 0,
        };
        return if char_len > len { len } else { 0 };
    }
    0
}

pub enum DecodedData<'a> {
    String(String),
    Binary(&'a [u8]),
}

impl Display for DecodedData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodedData::String(s) => f.write_str(s),
            DecodedData::Binary(data) => {
                for element in *data {
                    // Width of 4 allows 0xFF to be printed.
                    write!(f, "{element:#04x}").expect("Writing to String cannot fail");
                }
                Ok(())
            }
        }
    }
}

pub struct DefmtStateInner {
    pub table: defmt_decoder::Table,
    pub locs: Option<defmt_decoder::Locations>,
}

impl DefmtStateInner {
    pub fn try_from_bytes(buffer: &[u8]) -> anyhow::Result<Option<Self>> {
        let table = match defmt_decoder::Table::parse(buffer) {
            // Parsing ok, still no table found.
            Ok(None) => return Ok(None),
            Ok(Some(table)) => table,
            Err(e) => {
                tracing::warn!("failed to parse defmt data: {}", e);
                return Ok(None);
            }
        };

        let locs = table
            .get_locations(buffer)
            .with_context(|| "Failed to parse defmt data")?;

        let locs = if !table.is_empty() && locs.is_empty() {
            tracing::warn!(
                "Insufficient DWARF info; compile your program with `debug = 2` to enable location info."
            );
            None
        } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
            Some(locs)
        } else {
            tracing::warn!("Location info is incomplete; it will be omitted from the output.");
            None
        };
        Ok(Some(DefmtStateInner { table, locs }))
    }
}

/// defmt information common to all defmt channels.
#[derive(Clone)]
pub struct DefmtState {
    inner: Arc<DefmtStateInner>,
}
impl DefmtState {
    pub fn try_from_bytes(buffer: &[u8]) -> anyhow::Result<Option<Self>> {
        Ok(
            DefmtStateInner::try_from_bytes(buffer)?.map(|inner| DefmtState {
                inner: Arc::new(inner),
            }),
        )
    }

    fn as_ref(&self) -> &DefmtStateInner {
        &self.inner
    }

    pub fn table(&self) -> &defmt_decoder::Table {
        &self.inner.table
    }
}

impl fmt::Debug for DefmtState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefmtState").finish_non_exhaustive()
    }
}

pub struct DefmtProcessor {
    formatter: Formatter,
    // Fields are dropped in declaration order. `decoder` is holding a reference to defmt_data's
    // inner table, so it must be dropped first.
    decoder: Box<dyn DefmtStreamDecoder>,
    defmt_data: DefmtState,
}

impl DefmtProcessor {
    pub fn new(
        defmt_data: DefmtState,
        show_timestamps: bool,
        show_location: bool,
        log_format: Option<&str>,
    ) -> Self {
        let has_timestamp = defmt_data.table().has_timestamp();

        // Format options:
        // 1. Oneline format with optional location
        // 2. Custom format for the channel
        // 3. Default with optional location
        let format = match log_format {
            None | Some("oneline") => FormatterFormat::OneLine {
                with_location: show_location,
            },
            Some("full") => FormatterFormat::Default {
                with_location: show_location,
            },
            Some(format) => FormatterFormat::Custom(format),
        };

        Self {
            formatter: Formatter::new(FormatterConfig {
                format,
                is_timestamp_available: has_timestamp && show_timestamps,
            }),
            decoder: unsafe {
                // Extend lifetime to 'static. We can do this because we hold a reference to the
                // defmt_data's inner table for the lifetime of the processor.
                std::mem::transmute::<
                    Box<dyn DefmtStreamDecoder>,
                    Box<dyn DefmtStreamDecoder + 'static>,
                >(defmt_data.as_ref().table.new_stream_decoder())
            },
            defmt_data: defmt_data.clone(),
        }
    }

    /// Returns `true` if `data` starts a valid defmt stream for this table.
    fn decodes(&self, data: &[u8]) -> bool {
        let table = self.defmt_data.table();
        let mut decoder = table.new_stream_decoder();
        decoder.received(data);

        let mut valid = 0;
        let mut malformed = 0;
        while valid + malformed < 4 {
            match decoder.decode() {
                Ok(_) => valid += 1,
                Err(DecodeError::Malformed) if table.encoding().can_recover() => malformed += 1,
                Err(_) => break,
            }
        }
        valid > malformed
    }

    fn process(&mut self, buffer: &[u8]) -> anyhow::Result<String> {
        let DefmtStateInner { table, locs } = self.defmt_data.as_ref();
        self.decoder.received(buffer);

        let mut formatted_data = String::new();
        loop {
            match self.decoder.decode() {
                Ok(frame) => {
                    let loc = locs.as_ref().and_then(|locs| locs.get(&frame.index()));
                    let (file, line, module) = if let Some(loc) = loc {
                        (
                            loc.file.display().to_string(),
                            Some(loc.line.try_into().unwrap()),
                            Some(loc.module.as_str()),
                        )
                    } else {
                        (
                            format!(
                                "└─ <invalid location: defmt frame-index: {}>",
                                frame.index()
                            ),
                            None,
                            None,
                        )
                    };
                    let s = self
                        .formatter
                        .format_frame(frame, Some(&file), line, module);
                    writeln!(formatted_data, "{s}").expect("Writing to String cannot fail");
                }
                Err(DecodeError::UnexpectedEof) => break,
                Err(DecodeError::Malformed) if table.encoding().can_recover() => {
                    // If recovery is possible, skip the current frame and continue with new data.
                }
                Err(DecodeError::Malformed) => {
                    return Err(anyhow!(
                        "Unrecoverable error while decoding Defmt \
                        data. Some data may have been lost: {}",
                        DecodeError::Malformed
                    ));
                }
            }
        }

        Ok(formatted_data)
    }
}

/// The number of bytes to collect for a defmt check, if no frame ends earlier.
const DETECT_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetectedFormat {
    Text,
    /// A plain rzcobs defmt stream, as defmt-rtt sends.
    Defmt,
    Binary,
}

/// Decodes a stream whose format is not known in advance.
pub struct AutoDecoder {
    text: TextDecoder,
    /// `None` if the ELF has no rzcobs defmt table.
    defmt: Option<DefmtProcessor>,
    format: Option<DetectedFormat>,
    pending: Vec<u8>,
}

impl AutoDecoder {
    pub fn new(text: TextDecoder, defmt: Option<DefmtProcessor>) -> Self {
        // Only rzcobs frames can be found reliably.
        let defmt = defmt.filter(|processor| {
            processor.defmt_data.table().encoding() == defmt_decoder::Encoding::Rzcobs
        });
        Self {
            text,
            defmt,
            format: None,
            pending: Vec::new(),
        }
    }

    fn process(&mut self, buffer: &[u8]) -> anyhow::Result<String> {
        self.pending.extend_from_slice(buffer);

        let format = match self.format {
            Some(format) => format,
            None => {
                let Some(format) = self.detect() else {
                    return Ok(String::new());
                };
                tracing::debug!("Detected stream format: {format:?}");
                self.format = Some(format);
                format
            }
        };

        let pending = std::mem::take(&mut self.pending);
        match (format, self.defmt.as_mut()) {
            (DetectedFormat::Text, _) => Ok(self.text.process(&pending)),
            (DetectedFormat::Defmt, Some(defmt)) => defmt.process(&pending),
            _ => Ok(hex(&pending)),
        }
    }

    /// Returns `None` if more data is necessary.
    fn detect(&self) -> Option<DetectedFormat> {
        let data = self.pending.as_slice();

        if looks_like_text(data) {
            return Some(DetectedFormat::Text);
        }
        if let Some(defmt) = &self.defmt {
            if defmt.decodes(data) {
                return Some(DetectedFormat::Defmt);
            }
            // Wait for the end of the first frame.
            if !data.contains(&0) && data.len() < DETECT_LEN {
                return None;
            }
        }
        Some(DetectedFormat::Binary)
    }
}

fn hex(data: &[u8]) -> String {
    DecodedData::Binary(data).to_string()
}

/// Returns `true` if at most 10% of the characters are invalid UTF-8 or control characters.
fn looks_like_text(data: &[u8]) -> bool {
    let data = &data[..data.len() - incomplete_utf8_tail(data)];
    let text = String::from_utf8_lossy(data);
    let mut total = 0;
    let mut bad = 0;
    for c in text.chars() {
        total += 1;
        if c == char::REPLACEMENT_CHARACTER
            || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\x1b'))
        {
            bad += 1;
        }
    }
    bad * 10 <= total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(decoder: &mut StreamDecoder, chunks: &[&[u8]]) -> String {
        chunks
            .iter()
            .filter_map(|chunk| decoder.process(chunk).unwrap())
            .map(|data| data.to_string())
            .collect()
    }

    fn auto() -> StreamDecoder {
        StreamDecoder::Auto {
            decoder: AutoDecoder::new(TextDecoder::new(None, false), None),
        }
    }

    #[test]
    fn text_is_passed_through() {
        let mut decoder = auto();
        assert_eq!(
            decode(&mut decoder, &[b"Hello", b" world\nsecond line\n"]),
            "Hello world\nsecond line\n"
        );
    }

    #[test]
    fn short_text_is_shown_at_once() {
        let mut decoder = auto();
        assert_eq!(decode(&mut decoder, &[b"> "]), "> ");
    }

    #[test]
    fn split_utf8_character_is_kept() {
        let mut decoder = auto();
        let text = "árvíztűrő\n".as_bytes();
        assert_eq!(
            decode(&mut decoder, &[&text[..1], &text[1..2], &text[2..]]),
            "árvíztűrő\n"
        );
    }

    #[test]
    fn binary_is_shown_as_hex() {
        let mut decoder = auto();
        let data = [0x01u8, 0x02, 0x03, 0x80, 0x81, 0x00, 0x05, 0x06].repeat(4);
        assert_eq!(decode(&mut decoder, &[&data]), hex(&data));
    }
}
