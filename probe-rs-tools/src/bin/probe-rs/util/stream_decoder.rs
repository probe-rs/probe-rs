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
    String {
        /// UTC offset used for creating timestamps, if enabled.
        ///
        /// Getting the offset can fail in multi-threaded programs,
        /// so it needs to be stored.
        timestamp_offset: Option<UtcOffset>,
        last_line_done: bool,
        show_timestamps: bool,
    },
    BinaryLE,
    Defmt {
        processor: DefmtProcessor,
    },
    /// Zephyr dictionary-based logging.
    ZephyrDict {
        processor: ZephyrLogDecoder,
    },
}

impl From<&StreamDecoder> for DataFormat {
    fn from(config: &StreamDecoder) -> Self {
        match config {
            StreamDecoder::String { .. } => DataFormat::String,
            StreamDecoder::BinaryLE => DataFormat::BinaryLE,
            StreamDecoder::Defmt { .. } => DataFormat::Defmt,
            // The decoded output is text.
            StreamDecoder::ZephyrDict { .. } => DataFormat::String,
        }
    }
}

impl fmt::Debug for StreamDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamDecoder::String {
                timestamp_offset,
                last_line_done,
                show_timestamps,
            } => f
                .debug_struct("String")
                .field("timestamp_offset", timestamp_offset)
                .field("last_line_done", last_line_done)
                .field("show_timestamps", show_timestamps)
                .finish(),
            StreamDecoder::BinaryLE => f.debug_struct("BinaryLE").finish(),
            StreamDecoder::Defmt { .. } => f.debug_struct("Defmt").finish_non_exhaustive(),
            StreamDecoder::ZephyrDict { .. } => {
                f.debug_struct("ZephyrDict").finish_non_exhaustive()
            }
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

        let data = match self {
            StreamDecoder::BinaryLE => DecodedData::Binary(buffer),
            StreamDecoder::String {
                timestamp_offset,
                last_line_done,
                show_timestamps,
            } => {
                let string = Self::process_string(
                    buffer,
                    *timestamp_offset,
                    last_line_done,
                    *show_timestamps,
                )?;
                DecodedData::String(string)
            }
            StreamDecoder::Defmt { processor } => {
                let string = processor.process(buffer)?;

                DecodedData::String(string)
            }
            StreamDecoder::ZephyrDict { processor } => {
                DecodedData::String(processor.process(buffer))
            }
        };

        Ok(Some(data))
    }

    fn process_string(
        buffer: &[u8],
        offset: Option<UtcOffset>,
        last_line_done: &mut bool,
        show_timestamps: bool,
    ) -> anyhow::Result<String> {
        let incoming = String::from_utf8_lossy(buffer);

        if !show_timestamps {
            return Ok(incoming.to_string());
        }

        let Some(offset) = offset else {
            return Ok(incoming.to_string());
        };

        let timestamp = OffsetDateTime::now_utc()
            .to_offset(offset)
            .format(format_description!(
                "[hour repr:24]:[minute]:[second].[subsecond digits:3]"
            ))
            .expect("Incorrect format string. This shouldn't happen.");

        let mut formatted_data = String::new();
        for line in incoming.split_inclusive('\n') {
            if *last_line_done {
                write!(formatted_data, "{timestamp}: ").expect("Writing to String cannot fail");
            }
            write!(formatted_data, "{line}").expect("Writing to String cannot fail");
            *last_line_done = line.ends_with('\n');
        }
        Ok(formatted_data)
    }
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
