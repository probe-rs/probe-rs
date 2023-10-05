use std::fmt;

use probe_rs::rtt::{ChannelMode, DownChannel, UpChannel};
use probe_rs::Core;
use time::UtcOffset;
use time::{macros::format_description, OffsetDateTime};

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

#[derive(Debug)]
pub enum ChannelData {
    String(Vec<String>),
    Binary { data: Vec<u8>, is_defmt: bool },
}

impl ChannelData {
    pub fn new(format: DataFormat) -> Self {
        match format {
            DataFormat::String => Self::String(Vec::new()),
            DataFormat::BinaryLE | DataFormat::Defmt => Self::Binary {
                data: Vec::new(),
                is_defmt: format == DataFormat::Defmt,
            },
        }
    }

    fn clear(&mut self) {
        match self {
            Self::String(data) => data.clear(),
            Self::Binary { data, .. } => data.clear(),
        }
    }
}

#[derive(Debug)]
pub struct ChannelState {
    up_channel: Option<UpChannel>,
    down_channel: Option<DownChannel>,
    name: String,
    data: ChannelData,
    last_line_done: bool,
    input: String,
    scroll_offset: usize,
    rtt_buffer: RttBuffer,
    show_timestamps: bool,
}

impl ChannelState {
    pub fn new(
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        name: Option<String>,
        show_timestamps: bool,
        format: DataFormat,
    ) -> Self {
        let name = name
            .or_else(|| up_channel.as_ref().and_then(|up| up.name().map(Into::into)))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .and_then(|down| down.name().map(Into::into))
            })
            .unwrap_or_else(|| "Unnamed channel".to_owned());

        let data = ChannelData::new(format);

        Self {
            up_channel,
            down_channel,
            name,
            last_line_done: true,
            input: String::new(),
            scroll_offset: 0,
            rtt_buffer: RttBuffer([0u8; 1024]),
            show_timestamps,
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
            ChannelData::String(ref mut messages) => {
                let now = OffsetDateTime::now_utc().to_offset(offset);

                // First, convert the incoming bytes to UTF8.
                let mut incoming = String::from_utf8_lossy(&self.rtt_buffer.0[..count]).to_string();

                // Then pop the last stored line from our line buffer if possible and append our new line.
                let last_line_done = self.last_line_done;
                if !last_line_done {
                    if let Some(last_line) = messages.pop() {
                        incoming = last_line + &incoming;
                    }
                }
                self.last_line_done = incoming.ends_with('\n');

                // Then split the incoming buffer discarding newlines and if necessary
                // add a timestamp at start of each.
                // Note: this means if you print a newline in the middle of your debug
                // you get a timestamp there too..
                // Note: we timestamp at receipt of newline, not first char received if that
                // matters.
                for (i, line) in incoming.split_terminator('\n').enumerate() {
                    if self.show_timestamps && (last_line_done || i > 0) {
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
            // defmt output is later formatted into strings in [App::render].
            ChannelData::Binary { data, .. } => {
                data.extend_from_slice(&self.rtt_buffer.0[..count]);
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
