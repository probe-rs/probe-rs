use std::fmt;

use chrono::Local;
use probe_rs::Core;
use probe_rs_rtt::{ChannelMode, DownChannel, UpChannel};

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
pub struct ChannelState {
    up_channel: Option<UpChannel>,
    down_channel: Option<DownChannel>,
    name: String,
    format: DataFormat,
    /// Contains the strings when [ChannelState::format] is [DataFormat::String].
    messages: Vec<String>,
    /// When [ChannelState::format] is not [DataFormat::String] this
    /// contains RTT binary data or binary data in defmt format.
    data: Vec<u8>,
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

        Self {
            up_channel,
            down_channel,
            name,
            format,
            messages: Vec::new(),
            last_line_done: true,
            input: String::new(),
            scroll_offset: 0,
            rtt_buffer: RttBuffer([0u8; 1024]),
            show_timestamps,
            data: Vec::new(),
        }
    }

    pub fn has_down_channel(&self) -> bool {
        self.down_channel.is_some()
    }

    pub fn messages(&self) -> &Vec<String> {
        &self.messages
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

    pub fn format(&self) -> DataFormat {
        self.format
    }

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    pub fn clear(&mut self) {
        self.scroll_offset = 0;
        self.data = Vec::new();
        self.messages = Vec::new();
    }

    pub fn data(&self) -> &Vec<u8> {
        &self.data
    }

    /// Polls the RTT target for new data on the specified channel.
    ///
    /// Processes all the new data and adds it to the linebuffer of the respective channel.
    pub fn poll_rtt(&mut self, core: &mut Core) {
        // TODO: Proper error handling.
        let count = if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(core, self.rtt_buffer.0.as_mut()) {
                Ok(count) => count,
                Err(err) => {
                    log::error!("\nError reading from RTT: {}", err);
                    return;
                }
            }
        } else {
            0
        };

        if count == 0 {
            return;
        }

        match self.format {
            DataFormat::String => {
                let now = Local::now();

                // First, convert the incoming bytes to UTF8.
                let mut incoming = String::from_utf8_lossy(&self.rtt_buffer.0[..count]).to_string();

                // Then pop the last stored line from our line buffer if possible and append our new line.
                let last_line_done = self.last_line_done;
                if !last_line_done {
                    if let Some(last_line) = self.messages.pop() {
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
                        let ts = now.format("%H:%M:%S%.3f");
                        self.messages.push(format!("{} {}", ts, line));
                    } else {
                        self.messages.push(line.to_string());
                    }
                    if self.scroll_offset != 0 {
                        self.scroll_offset += 1;
                    }
                }
            }
            // defmt output is later formatted into strings in [App::render].
            DataFormat::BinaryLE | DataFormat::Defmt => {
                self.data.extend_from_slice(&self.rtt_buffer.0[..count]);
            }
        };
    }

    pub fn push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input += "\n";
            down_channel.write(core, self.input.as_bytes()).unwrap();
            self.input.clear();
        }
    }
}

struct RttBuffer([u8; 1024]);

impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
