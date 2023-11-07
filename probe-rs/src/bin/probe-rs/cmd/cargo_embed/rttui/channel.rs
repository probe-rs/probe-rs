use std::fmt;

use defmt_decoder::StreamDecoder;
use probe_rs::rtt::{ChannelMode, DownChannel, UpChannel};
use probe_rs::Core;
use time::UtcOffset;
use time::{macros::format_description, OffsetDateTime};

use crate::cmd::cargo_embed::DefmtInformation;

#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DataFormat {
    String,
    BinaryLE,
    Defmt,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelConfig {
    pub up: Option<usize>,
    pub down: Option<usize>,
    pub name: Option<String>,
    pub up_mode: Option<ChannelMode>,
    pub format: DataFormat,
}

pub enum ChannelData<'defmt> {
    String {
        data: Vec<String>,
        last_line_done: bool,
        show_timestamps: bool,
    },
    Binary {
        data: Vec<u8>,
    },
    Defmt {
        messages: Vec<String>,
        decoder: Box<dyn StreamDecoder + 'defmt>,
        information: &'defmt DefmtInformation,
    },
}

impl std::fmt::Debug for ChannelData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String {
                data,
                last_line_done,
                show_timestamps,
            } => f
                .debug_struct("String")
                .field("data", data)
                .field("last_line_done", last_line_done)
                .field("show_timestamps", show_timestamps)
                .finish(),
            Self::Binary { data } => f.debug_struct("Binary").field("data", data).finish(),
            Self::Defmt { messages, .. } => f
                .debug_struct("Defmt")
                .field("messages", messages)
                .finish_non_exhaustive(),
        }
    }
}

impl<'defmt> ChannelData<'defmt> {
    pub fn new_string(show_timestamps: bool) -> Self {
        Self::String {
            data: Vec::new(),
            last_line_done: true,
            show_timestamps,
        }
    }

    pub fn new_defmt(
        decoder: Box<dyn StreamDecoder + 'defmt>,
        information: &'defmt DefmtInformation,
    ) -> Self {
        Self::Defmt {
            messages: Vec::new(),
            decoder,
            information,
        }
    }

    pub fn new_binary() -> Self {
        Self::Binary { data: Vec::new() }
    }

    fn clear(&mut self) {
        match self {
            Self::String {
                data,
                last_line_done,
                show_timestamps: _,
            } => {
                data.clear();
                *last_line_done = true
            }
            Self::Binary { data, .. } => data.clear(),
            Self::Defmt { messages, .. } => messages.clear(),
        }
    }
}

#[derive(Debug)]
pub struct ChannelState<'defmt> {
    up_channel: Option<UpChannel>,
    down_channel: Option<DownChannel>,
    name: String,
    data: ChannelData<'defmt>,
    input: String,
    scroll_offset: usize,
    rtt_buffer: RttBuffer,
}

impl<'defmt> ChannelState<'defmt> {
    pub fn new(
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        name: Option<String>,
        data: ChannelData<'defmt>,
    ) -> Self {
        let name = name
            .or_else(|| up_channel.as_ref().and_then(|up| up.name().map(Into::into)))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .and_then(|down| down.name().map(Into::into))
            })
            .unwrap_or_else(|| "Unnamed channel".to_owned());

        Self {
            up_channel,
            down_channel,
            name,
            input: String::new(),
            scroll_offset: 0,
            rtt_buffer: RttBuffer([0u8; 1024]),
            data,
        }
    }

    pub fn has_down_channel(&self) -> bool {
        self.down_channel.is_some()
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut String {
        &mut self.input
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset += 1;
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    pub fn clear(&mut self) {
        self.scroll_offset = 0;

        self.data.clear();
    }

    /// Polls the RTT target for new data on the specified channel.
    ///
    /// Processes all the new data and adds it to the linebuffer of the respective channel.
    ///
    /// # Errors
    /// This function can return a [`time::Error`] if getting the local time or formatting a timestamp fails.
    pub fn poll_rtt(&mut self, core: &mut Core, offset: UtcOffset) -> Result<(), time::Error> {
        // TODO: Proper error handling.
        let count = if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(core, self.rtt_buffer.0.as_mut()) {
                Ok(count) => count,
                Err(err) => {
                    log::error!("\nError reading from RTT: {}", err);
                    return Ok(());
                }
            }
        } else {
            0
        };

        if count == 0 {
            return Ok(());
        }

        match &mut self.data {
            ChannelData::String {
                data: messages,
                last_line_done,
                show_timestamps,
            } => {
                let now = OffsetDateTime::now_utc().to_offset(offset);

                // First, convert the incoming bytes to UTF8.
                let mut incoming = String::from_utf8_lossy(&self.rtt_buffer.0[..count]).to_string();

                // Then pop the last stored line from our line buffer if possible and append our new line.
                if !*last_line_done {
                    if let Some(last_line) = messages.pop() {
                        incoming = last_line + &incoming;
                    }
                }
                *last_line_done = incoming.ends_with('\n');

                // Then split the incoming buffer discarding newlines and if necessary
                // add a timestamp at start of each.
                // Note: this means if you print a newline in the middle of your debug
                // you get a timestamp there too..
                // Note: we timestamp at receipt of newline, not first char received if that
                // matters.
                for (i, line) in incoming.split_terminator('\n').enumerate() {
                    if *show_timestamps && (*last_line_done || i > 0) {
                        let ts = now.format(format_description!(
                            "[hour repr:24]:[minute]:[second].[subsecond digits:3]"
                        ))?;
                        messages.push(format!("{ts} {line}"));
                    } else {
                        messages.push(line.to_string());
                    }
                    if self.scroll_offset != 0 {
                        self.scroll_offset += 1;
                    }
                }
            }
            ChannelData::Binary { data, .. } => {
                data.extend_from_slice(&self.rtt_buffer.0[..count]);
            }
            // defmt output is later formatted into strings in [App::render].
            ChannelData::Defmt {
                ref mut messages,
                ref mut decoder,
                information,
            } => {
                decoder.received(&self.rtt_buffer.0[..count]);
                while let Ok(frame) = decoder.decode() {
                    // NOTE(`[]` indexing) all indices in `table` have already been
                    // verified to exist in the `locs` map.
                    let loc: Option<_> = information
                        .location_information
                        .as_ref()
                        .map(|locs| &locs[&frame.index()]);

                    messages.push(format!("{}", frame.display(false)));
                    if let Some(loc) = loc {
                        let relpath = if let Ok(relpath) =
                            loc.file.strip_prefix(&std::env::current_dir().unwrap())
                        {
                            relpath
                        } else {
                            // not relative; use full path
                            &loc.file
                        };

                        messages.push(format!("└─ {}:{}", relpath.display(), loc.line));
                    }
                }
            }
        };

        Ok(())
    }

    pub fn push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input += "\n";
            down_channel.write(core, self.input.as_bytes()).unwrap();
            self.input.clear();
        }
    }

    pub(crate) fn data(&self) -> &ChannelData {
        &self.data
    }
}

struct RttBuffer([u8; 1024]);

impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
