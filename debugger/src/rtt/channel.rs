// [a, b, c, d, e, f, g, \n]
//
// t1: [a, b, c, d]
// t2: [e, f, g, \n]
// abcdefg

use std::{fmt, str::FromStr};

use chrono::{DateTime, Local};
use probe_rs_rtt::{DownChannel, UpChannel};
use structopt::StructOpt;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Packet {
    pub(crate) data_format: DataFormat,
    pub(crate) bytes: Vec<u8>,
    pub(crate) timestamp: DateTime<Local>,
}

#[derive(Debug, Copy, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DataFormat {
    String,
    BinaryLE,
    Defmt,
}

impl FromStr for DataFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let src = s.to_ascii_lowercase();
        match &src.to_ascii_lowercase()[..] { // A forgiving/case-insensitive match
            "string" => Ok(Self::String),
            "binaryle" => Ok(Self::BinaryLE),
            "defmt" => Ok(Self::Defmt),
            _ => Err(format!("{} is not a valid format", src)),
        }
    }
}

impl Default for DataFormat {
    fn default() -> Self {
        DataFormat::String
    }
}

#[derive(StructOpt, Debug, Clone, serde::Deserialize, Default)]
pub struct ChannelConfig {
    pub up: Option<usize>,
    pub down: Option<usize>,
    pub name: Option<String>,
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

    /// Returns the number of the UpChannel
    pub fn number(&self) -> Option<usize> {
        self.up_channel.as_ref().and_then(|uc|Some(uc.number()))
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

    pub fn data(&self) -> &Vec<u8> {
        &self.data
    }

    /// Polls the RTT target for new data on the specified channel.
    ///
    /// Processes all the new data and adds it to the linebuffer of the respective channel.
    pub fn poll_rtt(&mut self) -> Option<Packet> {
        // TODO: Proper error handling.
        let count = if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(self.rtt_buffer.0.as_mut()) {
                Ok(count) => count,
                Err(err) => {
                    log::error!("\nError reading from RTT: {}", err);
                    return None;
                }
            }
        } else {
            0
        };

        if count == 0 {
            return None;
        }

        match self.format {
            DataFormat::String | DataFormat::BinaryLE => Some(Packet {
                data_format: self.format,
                bytes: self.rtt_buffer.0[..count].to_vec(),
                timestamp: Local::now(),
            }),
            // defmt output is later formatted into strings in [App::render].
            DataFormat::Defmt => None,
        }
    }

    pub fn push_rtt(&mut self) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input += "\n";
            down_channel.write(&self.input.as_bytes()).unwrap();
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
