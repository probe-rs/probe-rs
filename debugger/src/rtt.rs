use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use probe_rs::Core;
use probe_rs_rtt::{DownChannel, UpChannel};
use serde::Deserialize;
use std::collections::HashMap;
use std::{
    fmt,
    io::{Read, Seek},
    str::FromStr,
};
use structopt::StructOpt;

/// Used by serde to provide defaults for `RttConfig`
fn default_channel_formats() -> Vec<RttChannelConfig> {
    vec![]
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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Packet {
    pub(crate) data_format: DataFormat,
    pub(crate) bytes: Vec<u8>,
    /// The packet will only contain a timestamp if the channel configuration selected it.
    pub(crate) timestamp: Option<DateTime<Local>>,
}
impl fmt::Display for Packet {
    /// This will write a formatted string for display to the RTT client.
    /// The timestamp is ONLY included if it is selected as an option, and will behave differently for Strings versus Binary. 
    /// - For `DataFormat::String`, each newline character is replaced with a timestamp
    /// - for `DataFormat::BinaryLE`, each `Packet` is pre-pended with a timestamp
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // First write the optional timestamp to the Formatter
        match self.data_format {
            DataFormat::String => { /// Optionally replace all newline characters with a timestamp
                if let Some(timestamp) = self.timestamp {
                    write!(f, "{} :", timestamp)?;
                }
                write!(f, "{}", String::from_utf8_lossy(&self.bytes).to_string())
            }
            DataFormat::BinaryLE => { /// Optionally put a timestamp before every packet read
                if let Some(timestamp) = self.timestamp {
                    write!(f, "{} :", timestamp)?;
                }
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

/// The initial configuration for RTT (Segger Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`
#[derive(StructOpt, Debug, Clone, Deserialize, Default)]
pub struct RttConfig {
    #[structopt(skip)]
    #[serde(rename = "rtt_enabled")]
    pub enabled: bool,
    /// Connection timeout in ms.
    #[structopt(skip)]
    #[serde(rename = "rtt_timeout")]
    pub timeout: usize,
    /// Configure data_format and show_timestamps for select channels
    #[structopt(skip)]
    #[serde(default = "default_channel_formats", rename = "rtt_channel_formats")]
    pub channels: Vec<RttChannelConfig>,
}
/// The User specified configuration for each active RTT Channel. The configuration is passed via a DAP Client configuration (`launch.json`). If no configuration is specified, the defaults will be Dataformat::String and show_timestamps=false
#[derive(StructOpt, Debug, Clone, serde::Deserialize, Default)]
pub struct RttChannelConfig {
    pub channel_number: Option<usize>,
    pub channel_name: Option<String>,
    #[serde(default)]
    pub data_format: DataFormat,
    #[structopt(skip)]
    #[serde(default)]
    pub show_timestamps: bool,
}

/// This is the primary interface through which RTT channel data is read and written. Every actual RTT channel has a configuration and buffer that is used for this purpose.
#[derive(Debug)]
pub struct RttActiveChannel {
    pub up_channel: Option<UpChannel>,
    pub down_channel: Option<DownChannel>,
    pub channel_name: String,
    pub data_format: DataFormat,
    /// Data that will be written to the down_channel (host to target)
    input_data: String,
    rtt_buffer: RttBuffer,
    show_timestamps: bool,
}

impl RttActiveChannel {
    pub fn new(
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        channel_config: Option<RttChannelConfig>,
    ) -> Self {
        let full_config = match channel_config {
            Some(channel_config) => channel_config,
            None => RttChannelConfig {
                ..Default::default()
            },
        };
        let name = up_channel
            .as_ref()
            .and_then(|up| up.name().map(Into::into))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .and_then(|down| down.name().map(Into::into))
            })
            .or_else(|| full_config.clone().channel_name)
            .unwrap_or_else(|| "Unnamed RTT channel".to_string());
        let buffer_size: usize = up_channel
            .as_ref()
            .and_then(|up| Some(up.buffer_size()))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .and_then(|down| Some(down.buffer_size()))
            })
            .unwrap_or_else(|| 1024); // This should never be the case ... 
        Self {
            up_channel,
            down_channel,
            channel_name: name,
            data_format: full_config.data_format,
            input_data: String::new(),
            rtt_buffer: RttBuffer::new(buffer_size),
            show_timestamps: full_config.show_timestamps,
        }
    }

    /// Returns the number of the UpChannel
    pub fn number(&self) -> Option<usize> {
        self.up_channel.as_ref().map(|uc| uc.number())
    }

    /// Polls the RTT target for new data on the specified channel.
    /// Processes all the new data and returns a `channel::Packet` with the appropriate data.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Option<Packet> {
        // TODO: Proper error handling.
        let count = if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(core, self.rtt_buffer.0.as_mut()) {
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
            data_format: self.data_format,
            bytes: self.rtt_buffer.0[..count].to_vec(),
            timestamp: if self.show_timestamps {
                Some(Local::now())
            } else {
                None
            },
        })
    }

    pub fn _push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input_data += "\n";
            down_channel
                .write(core, &self.input_data.as_bytes())
                .unwrap();
            self.input_data.clear();
        }
    }
}

struct RttBuffer(Vec<u8>);
impl RttBuffer {
    /// Initialize the buffer and ensure it has enough capacity to match the size of the RTT channel on the target at the time of instantiation. Doing this now prevents later performance impact if the buffer capacity has to be grown dynamically.
    pub fn new(mut buffer_size: usize) -> RttBuffer {
        let mut rtt_buffer = vec![0u8; 1];
        while buffer_size > 0 {
            buffer_size -= 1;
            rtt_buffer.push(0u8);
        };
        RttBuffer{0: rtt_buffer}
    }
}
impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Once an active connection with the Target RTT control block has been established, we configure each of the active channels, and hold essential state information for successfull communication.
pub struct RttActiveTarget {
    pub active_channels: Vec<RttActiveChannel>,
}

impl RttActiveTarget {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub fn new(mut rtt: probe_rs_rtt::Rtt, config: &RttConfig) -> Result<Self> {
        let mut active_channels = Vec::new();

        // For each channel configured in the RTT Control Block (`Rtt`), check if there are additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        let up_channels = rtt.up_channels().drain();
        let down_channels = rtt.down_channels().drain();
        for channel in up_channels.into_iter() {
            let number = channel.number();
            let channel_config = config
                .channels
                .clone()
                .into_iter()
                .find(|channel| channel.channel_number == Some(number));
            active_channels.push(RttActiveChannel::new(Some(channel), None, channel_config));
        }

        for channel in down_channels {
            let number = channel.number();
            let channel_config = config
                .channels
                .clone()
                .into_iter()
                .find(|channel| channel.channel_number == Some(number));
            active_channels.push(RttActiveChannel::new(None, Some(channel), channel_config));
        }

        // It doesn't make sense to pretend RTT is active, if there are no active channels
        if active_channels.is_empty() {
            return Err(anyhow!(
                "RTT Initialized correctly, but there were no active channels configured"
            ));
        }

        Ok(Self { active_channels })
    }

    pub fn get_rtt_symbol<T: Read + Seek>(file: &mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
            if let Ok(binary) = goblin::elf::Elf::parse(&buffer.as_slice()) {
                for sym in &binary.syms {
                    if let Some(name) = binary.strtab.get_at(sym.st_name) {
                        if name == "_SEGGER_RTT" {
                            return Some(sym.st_value);
                        }
                    }
                }
            }
        }

        log::warn!("No RTT header info was present in the ELF file. Does your firmware run RTT?");
        None
    }

    // pub fn render(
    //     &mut self,
    //     defmt_state: &Option<(defmt_decoder::Table, Option<defmt_decoder::Locations>)>,
    // ) {
    // binle_or_defmt => {
    //     self.terminal
    //         .draw(|f| {
    //             let constraints = if has_down_channel {
    //                 &[
    //                     Constraint::Length(1),
    //                     Constraint::Min(1),
    //                     Constraint::Length(1),
    //                 ][..]
    //             } else {
    //                 &[Constraint::Length(1), Constraint::Min(1)][..]
    //             };
    //             let chunks = Layout::default()
    //                 .direction(Direction::Vertical)
    //                 .margin(0)
    //                 .constraints(constraints)
    //                 .split(f.size());

    //             let tab_names = tabs
    //                 .iter()
    //                 .map(|t| Spans::from(t.name()))
    //                 .collect::<Vec<_>>();
    //             let tabs = Tabs::new(tab_names)
    //                 .select(current_tab)
    //                 .style(Style::default().fg(Color::Black).bg(Color::Yellow))
    //                 .highlight_style(
    //                     Style::default()
    //                         .fg(Color::Green)
    //                         .bg(Color::Yellow)
    //                         .add_modifier(Modifier::BOLD),
    //                 );
    //             f.render_widget(tabs, chunks[0]);

    //             height = chunks[1].height as usize;

    //             // probably pretty bad
    //             match binle_or_defmt {
    //                 DataFormat::BinaryLE => {
    //                     messages_wrapped.push(data.iter().fold(
    //                         String::new(),
    //                         |mut output, byte| {
    //                             let _ = write(&mut output, format_args!("{:#04x}, ", byte));
    //                             output
    //                         },
    //                     ));
    //                 }
    //                 DataFormat::Defmt => {
    //                     let (table, locs) = defmt_state.as_ref().expect(
    //                     "Running rtt in defmt mode but table or locations could not be loaded.",
    //                 );
    //                     let mut frames = vec![];

    //                     frames.extend_from_slice(&data);

    //                     while let Ok((frame, consumed)) =
    //                         table.decode(&frames)
    //                     {
    //                         // NOTE(`[]` indexing) all indices in `table` have already been
    //                         // verified to exist in the `locs` map.
    //                         let loc = locs.as_ref().map(|locs| &locs[&frame.index()]);

    //                         messages_wrapped.push(format!("{}", frame.display(false)));
    //                         if let Some(loc) = loc {
    //                             let relpath = if let Ok(relpath) =
    //                                 loc.file.strip_prefix(&std::env::current_dir().unwrap())
    //                             {
    //                                 relpath
    //                             } else {
    //                                 // not relative; use full path
    //                                 &loc.file
    //                             };

    //                             messages_wrapped.push(format!(
    //                                 "└─ {}:{}",
    //                                 relpath.display(),
    //                                 loc.line
    //                             ));
    //                         }

    //                         let num_frames = frames.len();
    //                         frames.rotate_left(consumed);
    //                         frames.truncate(num_frames - consumed);
    //                     }
    //                 }
    //                 DataFormat::String => unreachable!("You encountered a bug. Please open an issue on Github."),
    //             }

    //             let message_num = messages_wrapped.len();

    //             let messages: Vec<ListItem> = messages_wrapped
    //                 .iter()
    //                 .skip(message_num - (height + scroll_offset).min(message_num))
    //                 .take(height)
    //                 .map(|s| ListItem::new(vec![Spans::from(Span::raw(s))]))
    //                 .collect();

    //             let messages = List::new(messages.as_slice())
    //                 .block(Block::default().borders(Borders::NONE));
    //             f.render_widget(messages, chunks[1]);

    //             if has_down_channel {
    //                 let input = Paragraph::new(Spans::from(vec![Span::raw(input.clone())]))
    //                     .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
    //                 f.render_widget(input, chunks[2]);
    //             }
    //         })
    //         .unwrap();

    //     let message_num = messages_wrapped.len();
    //     let scroll_offset = self.tabs[self.current_tab].scroll_offset();
    //     if message_num < height + scroll_offset {
    //         self.current_tab_mut()
    //             .set_scroll_offset(message_num - height.min(message_num));
    //     }
    // }
    //}

    /// Polls the RTT target for new data on all channels.
    pub fn poll_rtt(&mut self, core: &mut Core) -> HashMap<String, Packet> {
        self.active_channels
            .iter_mut()
            .filter_map(|active_channel| {
                active_channel
                    .poll_rtt(core)
                    .map(|packet| (active_channel.number().unwrap_or(0).to_string(), packet))
                // If the Channel doesn't have a number, then send the output to channel 0
            })
            .collect::<HashMap<_, _>>()
    }

    // pub fn push_rtt(&mut self) {
    //     self.tabs[self.current_tab].push_rtt();
    // }
}
