use crate::*;
use anyhow::{anyhow, Result};
use chrono::Local;
use num_traits::Zero;
use probe_rs::Core;
use probe_rs_rtt::{DownChannel, UpChannel};
use serde::Deserialize;
use std::collections::HashMap;
use std::{
    fmt,
    fmt::Write,
    fs,
    io::{Read, Seek},
    str::FromStr,
};
use structopt::StructOpt;

/// Used by serde to provide defaults for `RttConfig`
fn default_channel_formats() -> Vec<RttChannelConfig> {
    vec![]
}

fn default_rtt_timeout() -> usize {
    500
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

/// The initial configuration for RTT (Real Time Transfer). This configuration is complimented with the additional information specified for each of the channels in `RttChannel`.
#[derive(StructOpt, Debug, Clone, Deserialize, Default)]
pub struct RttConfig {
    #[structopt(skip)]
    #[serde(default, rename = "rtt_enabled")]
    pub enabled: bool,
    /// Connection timeout in ms.
    #[structopt(skip)]
    #[serde(default = "default_rtt_timeout", rename = "rtt_timeout")]
    pub timeout: usize,
    /// Configure data_format and show_timestamps for select channels
    #[structopt(skip)]
    #[serde(default = "default_channel_formats", rename = "rtt_channel_formats")]
    pub channels: Vec<RttChannelConfig>,
}

/// The User specified configuration for each active RTT Channel. The configuration is passed via a DAP Client configuration (`launch.json`). If no configuration is specified, the defaults will be `Dataformat::String` and `show_timestamps=false`.
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

/// A fully configured RttActiveChannel. The configuration will always try to 'default' based on information read from the RTT control block in the binary. Where insufficient information is available, it will use the supplied configuration, with final hardcoded defaults where no other information was available.
impl RttActiveChannel {
    pub fn new(
        up_channel: Option<UpChannel>,
        down_channel: Option<DownChannel>,
        channel_config: Option<RttChannelConfig>,
    ) -> Self {
        let full_config = match channel_config {
            Some(channel_config) => channel_config,
            None => RttChannelConfig {
                ..Default::default() // Will set intelligent defaults below ...
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
            .map(|up| up.buffer_size())
            .or_else(|| down_channel.as_ref().map(|down| down.buffer_size()))
            .unwrap_or(1024); // This should never be the case ...
        let defmt_enabled: bool = up_channel
            .as_ref()
            .map(|up| up.name() == Some("defmt"))
            .or_else(|| {
                down_channel
                    .as_ref()
                    .map(|down| down.name() == Some("defmt"))
            })
            .unwrap_or(false); // This should never be the case ...
        let data_format: DataFormat = if defmt_enabled {
            DataFormat::Defmt
        } else {
            full_config.data_format
        };
        Self {
            up_channel,
            down_channel,
            channel_name: name,
            data_format,
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
    /// Processes all the new data into the channel `rtt_buffer` and returns the number of bytes that was read
    pub fn poll_rtt(&mut self, core: &mut Core) -> Option<usize> {
        if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(core, self.rtt_buffer.0.as_mut()) {
                Ok(count) => {
                    if count.is_zero() {
                        None
                    } else {
                        Some(count)
                    }
                }
                Err(err) => {
                    log::error!("\nError reading from RTT: {}", err);
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn _push_rtt(&mut self, core: &mut Core) {
        if let Some(down_channel) = self.down_channel.as_mut() {
            self.input_data += "\n";
            down_channel
                .write(core, self.input_data.as_bytes())
                .unwrap();
            self.input_data.clear();
        }
    }
}

/// Once an active connection with the Target RTT control block has been established, we configure each of the active channels, and hold essential state information for successfull communication.
#[derive(Debug)]
pub struct RttActiveTarget {
    pub active_channels: Vec<RttActiveChannel>,
    defmt_state: Option<(defmt_decoder::Table, Option<defmt_decoder::Locations>)>,
}

impl RttActiveTarget {
    /// RttActiveTarget collects references to all the `RttActiveChannel`s, for latter polling/pushing of data.
    pub fn new(mut rtt: probe_rs_rtt::Rtt, debugger_options: &DebuggerOptions) -> Result<Self> {
        let mut active_channels = Vec::new();
        // For each channel configured in the RTT Control Block (`Rtt`), check if there are additional user configuration in a `RttChannelConfig`. If not, apply defaults.
        let up_channels = rtt.up_channels().drain();
        let down_channels = rtt.down_channels().drain();
        for channel in up_channels.into_iter() {
            let number = channel.number();
            let channel_config = debugger_options
                .rtt
                .channels
                .clone()
                .into_iter()
                .find(|channel| channel.channel_number == Some(number));
            active_channels.push(RttActiveChannel::new(Some(channel), None, channel_config));
        }

        for channel in down_channels {
            let number = channel.number();
            let channel_config = debugger_options
                .rtt
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

        let defmt_enabled = active_channels
            .iter()
            .any(|elem| elem.data_format == DataFormat::Defmt);
        let defmt_state = if defmt_enabled {
            let elf = fs::read(debugger_options.program_binary.clone().unwrap()) // We can safely unwrap() program_binary here, because it is validated to exist at startup of the debugger
                .map_err(|err| {
                    anyhow!(
                        "Error reading program binary while initalizing RTT: {}",
                        err
                    )
                })?;
            if let Some(table) = defmt_decoder::Table::parse(&elf)? {
                let locs = {
                    let locs = table.get_locations(&elf)?;

                    if !table.is_empty() && locs.is_empty() {
                        log::warn!("Insufficient DWARF info; compile your program with `debug = 2` to enable location info.");
                        None
                    } else if table.indices().all(|idx| locs.contains_key(&(idx as u64))) {
                        Some(locs)
                    } else {
                        log::warn!(
                            "Location info is incomplete; it will be omitted from the output."
                        );
                        None
                    }
                };
                Some((table, locs))
            } else {
                log::warn!("No `Table` definition in DWARF info; compile your program with `debug = 2` to enable location info.");
                None
            }
        } else {
            None
        };

        Ok(Self {
            active_channels,
            defmt_state,
        })
    }

    pub fn get_rtt_symbol<T: Read + Seek>(file: &mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
            if let Ok(binary) = goblin::elf::Elf::parse(buffer.as_slice()) {
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

    /// Polls the RTT target for new data on all channels.
    pub fn poll_rtt(&mut self, core: &mut Core) -> HashMap<String, String> {
        let defmt_state = self.defmt_state.as_ref();
        self.active_channels
            .iter_mut()
            .filter_map(|active_channel| {
                 active_channel
                    .poll_rtt(core)
                    .map(|bytes_read| {
                        (
                            active_channel.number().unwrap_or(0).to_string(), // If the Channel doesn't have a number, then send the output to channel 0
                            {
                                let mut formatted_data = String::new();
                                match active_channel.data_format {
                                    DataFormat::String => {
                                        let incoming = String::from_utf8_lossy(&active_channel.rtt_buffer.0[..bytes_read]).to_string();
                                        for (_i, line) in incoming.split_terminator('\n').enumerate() {
                                            if active_channel.show_timestamps {
                                                write!(formatted_data, "{} :", Local::now())
                                                    .map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                            }
                                            writeln!(formatted_data, "{}", line).map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                        }
                                    }
                                    DataFormat::BinaryLE => {
                                        for element in &active_channel.rtt_buffer.0[..bytes_read] {
                                            write!(formatted_data, "{:#04x}", element).map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r); //Width of 4 allows 0xFF to be printed.
                                        }
                                        // write!(formatted_data, "").map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                    }
                                    DataFormat::Defmt => {
                                        match defmt_state {
                                            Some((table, locs)) => {
                                                let mut frames = vec![];
                                                frames.extend_from_slice(&active_channel.rtt_buffer.0[..bytes_read]);

                                                while let Ok((frame, consumed)) =
                                                    table.decode(&frames)
                                                {
                                                    // NOTE(`[]` indexing) all indices in `table` have already been
                                                    // verified to exist in the `locs` map.
                                                    let loc = locs.as_ref().map(|locs| &locs[&frame.index()]);

                                                    writeln!(formatted_data, "{}", frame.display(false)).map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                                    if let Some(loc) = loc {
                                                        let relpath = if let Ok(relpath) =
                                                            loc.file.strip_prefix(&std::env::current_dir().unwrap())
                                                        {
                                                            relpath
                                                        } else {
                                                            // not relative; use full path
                                                            &loc.file
                                                        };
                                                        writeln!(formatted_data,
                                                            "└─ {}:{}",
                                                            relpath.display(),
                                                            loc.line
                                                        ).map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                                    }

                                                    let num_frames = frames.len();
                                                    frames.rotate_left(consumed);
                                                    frames.truncate(num_frames - consumed);
                                                }
                                            }
                                            None => {
                                                write!(formatted_data, "Running rtt in defmt mode but table or locations could not be loaded.")
                                                    .map_or_else(|err| log::error!("Failed to format RTT data - {:?}", err), |r|r);
                                            }
                                        }
                                    }
                                };
                                formatted_data
                            }
                        )
                    })
            })
            .collect::<HashMap<_, _>>()
    }

    // pub fn push_rtt(&mut self) {
    //     self.tabs[self.current_tab].push_rtt();
    // }
}

struct RttBuffer(Vec<u8>);
impl RttBuffer {
    /// Initialize the buffer and ensure it has enough capacity to match the size of the RTT channel on the target at the time of instantiation. Doing this now prevents later performance impact if the buffer capacity has to be grown dynamically.
    pub fn new(mut buffer_size: usize) -> RttBuffer {
        let mut rtt_buffer = vec![0u8; 1];
        while buffer_size > 0 {
            buffer_size -= 1;
            rtt_buffer.push(0u8);
        }
        RttBuffer { 0: rtt_buffer }
    }
}
impl fmt::Debug for RttBuffer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}
