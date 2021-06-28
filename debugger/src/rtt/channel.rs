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
    /// The packet will only contain a timestamp if the channel configuration selected it.
    pub(crate) timestamp: Option<DateTime<Local>>,
}

impl fmt::Display for Packet {
    /// This will write a formatted string for display to the RTT client.
    /// The timestamp is ONLY included if it is selected as an option AND when `Packet::data_format` is `DataFormat::String`
    /// TODO: The current implementation assumes that every packet is a self contained user message, even though RTT doesn't work that way.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // First write the optional timestamp to the Formatter
        match self.data_format {
            DataFormat::String => {
                if let Some(timestamp) = self.timestamp {
                    write!(f, "{} :", timestamp)?;
                }
                write!(f, "{}", String::from_utf8_lossy(&self.bytes).to_string())
            }
            DataFormat::BinaryLE => {
                for element in self.bytes.clone() {
                    write!(f, "{:#04x}", element)?; //Width of 4 allows 0xFF to be printed.
                }
                write!(f, "")
            }
            DataFormat::Defmt => {
                write!(f, "{:?}", self.bytes)
            }
        }
    }
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
        match &src.to_ascii_lowercase()[..] {
            // A forgiving/case-insensitive match
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
    input: String,
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
            input: String::new(),
            rtt_buffer: RttBuffer([0u8; 1024]),
            show_timestamps,
        }
    }

    /// Returns the number of the UpChannel
    pub fn number(&self) -> Option<usize> {
        self.up_channel.as_ref().and_then(|uc| Some(uc.number()))
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

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn format(&self) -> DataFormat {
        self.format
    }

    /// Polls the RTT target for new data on the specified channel.
    /// Processes all the new data and returns a `channel::Packet` with the appropriate data.
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

        Some(Packet {
            data_format: self.format,
            bytes: self.rtt_buffer.0[..count].to_vec(),
            timestamp: if self.show_timestamps {
                Some(Local::now())
            } else {
                None
            },
        })
    }

    pub fn push_rtt(&mut self) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input += "\n";
            down_channel.write(&self.input.as_bytes()).unwrap();
            self.input.clear();
        }
    }
}

struct RttBuffer([u8; 1024]); // TODO: RttBuffer is hardcoded at 1024.

impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
