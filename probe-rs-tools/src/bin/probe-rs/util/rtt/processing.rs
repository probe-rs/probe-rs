use anyhow::{Context, anyhow};
use defmt_decoder::{
    DecodeError, StreamDecoder,
    log::format::{Formatter, FormatterConfig, FormatterFormat},
};
use probe_rs::rtt::Error;
use time::{OffsetDateTime, UtcOffset, macros::format_description};

use std::{
    fmt::{self, Write},
    sync::Arc,
};

use crate::util::rtt::DataFormat;

pub enum RttDecoder {
    String {
        /// UTC offset used for creating timestamps, if enabled.
        ///
        /// Getting the offset can fail in multi-threaded programs,
        /// so it needs to be stored.
        timestamp_offset: Option<UtcOffset>,
        last_line_done: bool,
    },
    BinaryLE,
    Defmt {
        processor: DefmtProcessor,
    },
}

impl From<&RttDecoder> for DataFormat {
    fn from(config: &RttDecoder) -> Self {
        match config {
            RttDecoder::String { .. } => DataFormat::String,
            RttDecoder::BinaryLE => DataFormat::BinaryLE,
            RttDecoder::Defmt { .. } => DataFormat::Defmt,
        }
    }
}

impl fmt::Debug for RttDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RttDecoder::String {
                timestamp_offset,
                last_line_done,
            } => f
                .debug_struct("String")
                .field("timestamp_offset", timestamp_offset)
                .field("last_line_done", last_line_done)
                .finish(),
            RttDecoder::BinaryLE => f.debug_struct("BinaryLE").finish(),
            RttDecoder::Defmt { .. } => f.debug_struct("Defmt").finish_non_exhaustive(),
        }
    }
}

impl RttDecoder {
    /// Returns whether the channel is expected to output binary data (`true`)
    /// or human-readable strings (`false`).
    pub fn is_binary(&self) -> bool {
        matches!(self, RttDecoder::BinaryLE)
    }

    pub fn process(
        &mut self,
        buffer: &[u8],
        collector: &mut impl RttDataHandler,
    ) -> Result<(), Error> {
        // Prevent the format processors generating empty strings.
        if buffer.is_empty() {
            return Ok(());
        }

        match self {
            RttDecoder::BinaryLE => collector.on_binary_data(buffer),
            RttDecoder::String {
                timestamp_offset,
                last_line_done,
            } => {
                let string = Self::process_string(buffer, *timestamp_offset, last_line_done)?;
                collector.on_string_data(string)
            }
            RttDecoder::Defmt { processor } => {
                let string = processor.process(buffer)?;
                collector.on_string_data(string)
            }
        }
    }

    fn process_string(
        buffer: &[u8],
        offset: Option<UtcOffset>,
        last_line_done: &mut bool,
    ) -> Result<String, Error> {
        let incoming = String::from_utf8_lossy(buffer);

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

pub trait RttDataHandler {
    fn on_binary_data(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut formatted_data = String::with_capacity(data.len() * 4);
        for element in data {
            // Width of 4 allows 0xFF to be printed.
            write!(&mut formatted_data, "{element:#04x}").expect("Writing to String cannot fail");
        }
        self.on_string_data(formatted_data)
    }

    fn on_string_data(&mut self, data: String) -> Result<(), Error>;
}

pub struct DefmtStateInner {
    pub table: defmt_decoder::Table,
    pub locs: Option<defmt_decoder::Locations>,
}

impl DefmtStateInner {
    pub fn try_from_bytes(buffer: &[u8]) -> Result<Option<Self>, Error> {
        let Some(table) =
            defmt_decoder::Table::parse(buffer).with_context(|| "Failed to parse defmt data")?
        else {
            return Ok(None);
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
    pub fn try_from_bytes(buffer: &[u8]) -> Result<Option<Self>, Error> {
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
    decoder: Box<dyn StreamDecoder>,
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
                std::mem::transmute::<Box<dyn StreamDecoder>, Box<dyn StreamDecoder + 'static>>(
                    defmt_data.as_ref().table.new_stream_decoder(),
                )
            },
            defmt_data: defmt_data.clone(),
        }
    }

    fn process(&mut self, buffer: &[u8]) -> Result<String, Error> {
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
                    return Err(Error::Other(anyhow!(
                        "Unrecoverable error while decoding Defmt \
                        data. Some data may have been lost: {}",
                        DecodeError::Malformed
                    )));
                }
            }
        }

        Ok(formatted_data)
    }
}
