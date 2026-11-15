//! Decodes the target output, and writes it to the terminal and to files.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use time::UtcOffset;
use tokio::io::AsyncWriteExt;

use crate::cmd::run::MonitoringOptions;
use crate::rpc::Key;
use crate::rpc::RttClient;
use crate::util::stream_decoder::{
    AutoDecoder, DefmtProcessor, DefmtState, StreamDecoder, TextDecoder,
};
use probe_rs_rpc::monitor::ChannelInfo;
use probe_rs_rpc::monitor::{RttEvent, SemihostingEvent, SerialEvent};
use probe_rs_rpc::rtt_config::RttChannelConfig;
use probe_rs_rpc_client::{MonitorEvent, SessionInterface};
use probe_rs_zephyr::log::{Database as ZephyrLogDatabase, Decoder as ZephyrLogDecoder};

use super::FileMetadata;

type TargetOutputFiles = std::collections::HashMap<ChannelIdentifier, tokio::fs::File>;

/// A selector for a named stream, be it an RTT or a semihosting channel.
///
/// When converting from text (eg. as a CLI argument), the `Unqualified` variant is only produced
/// when there is no colon in the name; otherwise, the prefix before the colon is matched into a
/// variant.
///
/// ```
/// assert_eq!(ChannelIdentifier::Unqualified("foo".to_string()), "foo".parse().unwrap());
/// assert_eq!(ChannelIdentifier::Rtt("defmt".to_string()), "rtt:defmt".parse().unwrap());
/// assert_eq!(ChannelIdentifier::CatchAll, "".parse().unwrap());
/// ```
// Could we be smart with the Strings and implement this for any type and then do some AsRef and
// the right tricks to access a hashmap keyed with an owned identifier with a borrowed one? Maybe.
// But allocators are fast, this won't be a bottleneck, and it is easy to maintain with
// always-owned channels.
#[derive(PartialEq, Eq, Hash, Clone)]
pub(crate) enum ChannelIdentifier {
    /// A named channel (might match a semihosting or an RTT channel)
    Unqualified(String),
    /// A named RTT channel
    Rtt(String),
    /// A named semihosting channel
    Semihosting(String),
    /// Selector that matches any channel; depending on the context, this usually means "any
    /// channel that is not explicitly handled".
    CatchAll,
}

impl std::str::FromStr for ChannelIdentifier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            [] => unreachable!(),
            [""] => ChannelIdentifier::CatchAll,
            [unqualified] => ChannelIdentifier::Unqualified(unqualified.to_string()),
            ["rtt", rtt] => ChannelIdentifier::Rtt(rtt.to_string()),
            ["semihosting", semihosting] => ChannelIdentifier::Semihosting(semihosting.to_string()),
            _ => anyhow::bail!(
                "Channel identifiers with colons need to be qualified as `rtt:name` or `semihosting:name`."
            ),
        })
    }
}

impl ChannelIdentifier {
    /// Returns an `Unqualified(name)` for any qualified name.
    fn unqualified(&self) -> Option<Self> {
        match self {
            ChannelIdentifier::Rtt(n) => Some(ChannelIdentifier::Unqualified(n.clone())),
            ChannelIdentifier::Semihosting(n) => Some(ChannelIdentifier::Unqualified(n.clone())),
            _ => None,
        }
    }

    /// Picks a channel out of a map of channels, falling back to using an unqualified version of
    /// the same value, or the map's catch-all channel.
    fn find_in<'res, T>(
        &self,
        map: &'res mut std::collections::HashMap<Self, T>,
    ) -> Option<&'res mut T> {
        // This double/triple access (get / get_mut) is a bit weird, but the compiler will not see
        // that the lifetimes of the get_mut are non-overlapping if we return the Ok of an initial
        // get_mut.
        if map.contains_key(self) {
            return map.get_mut(self);
        };
        if let Some(fallback) = self.unqualified()
            && map.contains_key(&fallback)
        {
            return map.get_mut(&fallback);
        };
        map.get_mut(&Self::CatchAll)
    }
}

/// Splits argument text strings like `['channel1=file-for-c1', 'stdout=some-file', 'defaultfile']` by the
/// `=` signs, mapping the keys to a [`ChannelIdentifier`] (or
/// [`CatchAll`][ChannelIdentifier::CatchAll] when no key present) and opening the values as files
/// in append mode.
pub(crate) async fn connect_target_output_files(
    arg: &[String],
) -> anyhow::Result<TargetOutputFiles> {
    let mut map = TargetOutputFiles::new();
    for component in arg {
        let parts: Vec<&str> = component.splitn(2, "=").collect();
        let (key, value) = match parts[..] {
            // Tolerating empty entries in particular makes a trailing comma tolerated.
            [] => continue,
            [single] => (ChannelIdentifier::CatchAll, single),
            [first, second] => (first.parse()?, second),
            _ => unreachable!("splitn produces at most 2 items."),
        };
        let value = tokio::fs::OpenOptions::new()
            .read(false)
            .append(true)
            .create(true)
            .open(value)
            .await?;
        map.insert(key, value);
    }
    Ok(map)
}

pub async fn rtt_client(
    session: &SessionInterface,
    meta: &FileMetadata,
    monitor_options: &MonitoringOptions,
) -> anyhow::Result<CliRttClient> {
    let scan_regions = match &meta.scan_regions {
        Some(scan_regions) => scan_regions.clone(),
        None => monitor_options.scan_region.clone(),
    };

    let zephyr_log_dictionary = match &monitor_options.zephyr_log_dictionary {
        Some(path) => {
            let data = tokio::fs::read(path).await.with_context(|| {
                format!(
                    "Failed to read Zephyr log dictionary from {}",
                    path.display()
                )
            })?;
            Some(Arc::new(ZephyrLogDatabase::from_bytes(&data)?))
        }
        None => meta.zephyr_log_dictionary.clone(),
    };

    // We don't really know what to configure here, so we set a default configuration if we can, but that's it.
    let rtt_client = session
        .create_rtt_client(
            scan_regions,
            vec![],
            RttChannelConfig {
                mode: Some(monitor_options.rtt_channel_mode),
                ..Default::default()
            },
        )
        .await?;

    // The actual data processor objects will be created once we have the channel names.
    Ok(CliRttClient {
        handle: rtt_client.handle,
        channel_processors: vec![],
        zephyr_log_dictionary,
    })
}

pub struct CliRttClient {
    handle: Key<RttClient>,
    channel_processors: Vec<Channel>,

    // Data necessary to create the channel processors once we know the channel names.
    zephyr_log_dictionary: Option<Arc<ZephyrLogDatabase>>,
}

/// The settings to create the decoders of the target output.
#[derive(Default)]
pub struct DecoderSettings {
    log_format: Option<String>,
    show_timestamps: bool,
    show_location: bool,
    timestamp_offset: Option<UtcOffset>,
    defmt_data: Option<DefmtState>,
}

impl DecoderSettings {
    pub fn new(
        meta: &FileMetadata,
        monitor_options: &MonitoringOptions,
        timestamp_offset: Option<UtcOffset>,
    ) -> Self {
        Self {
            log_format: monitor_options.log_format.clone(),
            show_timestamps: !monitor_options.no_timestamps,
            show_location: !monitor_options.no_location,
            timestamp_offset,
            defmt_data: meta.defmt_data.clone(),
        }
    }

    fn defmt_processor(&self) -> Option<DefmtProcessor> {
        self.defmt_data.clone().map(|defmt_data| {
            DefmtProcessor::new(
                defmt_data,
                self.show_timestamps,
                self.show_location,
                self.log_format.as_deref(),
            )
        })
    }

    fn auto_decoder(&self) -> StreamDecoder {
        StreamDecoder::Auto {
            decoder: AutoDecoder::new(
                TextDecoder::new(self.timestamp_offset, self.show_timestamps),
                self.defmt_processor(),
            ),
        }
    }
}

/// The decoders of the target output: RTT, semihosting and the serial port to the target.
pub struct TargetOutput {
    decoders: DecoderSettings,
    rtt_client: Option<CliRttClient>,
    semihosting_streams: HashMap<String, Channel>,
    serial: Channel,
    files: TargetOutputFiles,
    /// The RTT up channels to show. Empty shows all channels.
    up_channels: Vec<u32>,
}

impl TargetOutput {
    pub fn new(decoders: DecoderSettings, rtt_client: Option<CliRttClient>) -> Self {
        Self {
            serial: Channel::new("serial".to_string(), decoders.auto_decoder()),
            decoders,
            rtt_client,
            semihosting_streams: HashMap::new(),
            files: TargetOutputFiles::new(),
            up_channels: Vec::new(),
        }
    }

    /// Opens the output files and selects the up channels that `monitor_options` name.
    pub(super) async fn route(
        &mut self,
        monitor_options: &MonitoringOptions,
    ) -> anyhow::Result<()> {
        self.files = connect_target_output_files(&monitor_options.target_output_file).await?;
        self.up_channels = monitor_options.rtt_up_channels.clone();
        Ok(())
    }

    pub(super) fn rtt_handle(&self) -> Option<Key<RttClient>> {
        self.rtt_client.as_ref().map(CliRttClient::handle)
    }

    /// Writes the data that waits in the decoders for more input.
    pub(super) async fn flush(
        &mut self,
        shared_writer: &impl AsyncFn(&str),
        error_writer: &impl AsyncFn(&str),
    ) {
        if let Some(client) = &mut self.rtt_client {
            for processor in &mut client.channel_processors {
                let output_file =
                    ChannelIdentifier::Rtt(processor.channel.clone()).find_in(&mut self.files);
                processor.flush(shared_writer, output_file).await;
            }
        }
        for (stream, channel) in &mut self.semihosting_streams {
            let output_file =
                ChannelIdentifier::Semihosting(stream.clone()).find_in(&mut self.files);
            let writer = semihosting_writer(stream, shared_writer, error_writer);
            channel.flush(&writer, output_file).await;
        }
        let output_file =
            ChannelIdentifier::Unqualified("serial".to_string()).find_in(&mut self.files);
        self.serial.flush(shared_writer, output_file).await;
    }

    pub(super) async fn handle_event(
        &mut self,
        event: MonitorEvent,
        shared_writer: &impl AsyncFn(&str),
        error_writer: &impl AsyncFn(&str),
    ) {
        match event {
            MonitorEvent::Rtt(RttEvent::Discovered { up_channels, .. }) => {
                if let Some(client) = &mut self.rtt_client {
                    client.on_channels_discovered(&up_channels, &self.decoders);
                }
            }
            MonitorEvent::Rtt(RttEvent::Output { channel, bytes }) => {
                self.write_rtt(channel, &bytes, shared_writer).await;
            }
            MonitorEvent::Semihosting(SemihostingEvent::Output { stream, bytes }) => {
                let output_file =
                    ChannelIdentifier::Semihosting(stream.clone()).find_in(&mut self.files);

                let writer = semihosting_writer(&stream, shared_writer, error_writer);
                let channel = self
                    .semihosting_streams
                    .entry(stream.clone())
                    .or_insert_with(|| Channel::new(stream.clone(), self.decoders.auto_decoder()));
                channel.process(&bytes, &writer, output_file).await;
            }
            MonitorEvent::Serial(SerialEvent::Output { bytes }) => {
                let output_file =
                    ChannelIdentifier::Unqualified("serial".to_string()).find_in(&mut self.files);

                self.serial
                    .process(&bytes, shared_writer, output_file)
                    .await;
            }
        }
    }

    async fn write_rtt(&mut self, channel: u32, bytes: &[u8], shared_writer: &impl AsyncFn(&str)) {
        if !self.up_channels.is_empty() && !self.up_channels.contains(&channel) {
            return;
        }
        let Some(processor) = self
            .rtt_client
            .as_mut()
            .and_then(|client| client.channel_processors.get_mut(channel as usize))
        else {
            return;
        };

        processor
            .process(
                bytes,
                shared_writer,
                // See ChannelIdentifier on why we access with clones here; also, while it'd be
                // more efficient to resolve those lookups at channel discovery, it doesn't really
                // matter, and again, ease of maintenance beats theoretical performance unless
                // benchmarked otherwise.
                ChannelIdentifier::Rtt(processor.channel.clone()).find_in(&mut self.files),
            )
            .await;
    }
}

/// Returns the writer for the semihosting `stream`: `error_writer` for `stderr`, `shared_writer`
/// for the other streams.
fn semihosting_writer(
    stream: &str,
    shared_writer: &impl AsyncFn(&str),
    error_writer: &impl AsyncFn(&str),
) -> impl AsyncFn(&str) {
    let is_stderr = stream == "stderr";
    async move |message: &str| {
        if is_stderr {
            error_writer(message).await
        } else {
            shared_writer(message).await
        }
    }
}

impl CliRttClient {
    pub fn handle(&self) -> Key<RttClient> {
        self.handle
    }

    fn on_channels_discovered(&mut self, up_channels: &[ChannelInfo], decoders: &DecoderSettings) {
        // Already configured.
        if !self.channel_processors.is_empty() {
            return;
        }

        // Apply our heuristics based on channel names.
        let processors = up_channels
            .iter()
            .enumerate()
            .map(|(number, channel)| {
                Channel::new(
                    channel.name.clone(),
                    self.decoder(number, channel, decoders),
                )
            })
            .collect();
        self.channel_processors = processors;

        // If there are multiple channels, print the channel names.
        if up_channels.len() > 1 {
            let width = up_channels.iter().map(|c| c.name.len()).max().unwrap();
            for processor in self.channel_processors.iter_mut() {
                processor.print_channel_name(width);
            }
        }
    }

    /// Selects the decoder for the up channel with index `number`.
    fn decoder(
        &self,
        number: usize,
        channel: &ChannelInfo,
        decoders: &DecoderSettings,
    ) -> StreamDecoder {
        // Zephyr outputs dictionary-based logs on a single channel, which the database records.
        if let Some(db) = &self.zephyr_log_dictionary
            && db.rtt_channel().unwrap_or(0) as usize == number
        {
            return StreamDecoder::ZephyrDict {
                processor: ZephyrLogDecoder::new(db.clone()),
            };
        }

        if channel.name != "defmt" {
            return decoders.auto_decoder();
        }
        match decoders.defmt_processor() {
            Some(processor) => StreamDecoder::Defmt { processor },
            // Not much we can do. Don't silently eat the data.
            None => StreamDecoder::BinaryLE,
        }
    }
}

struct Channel {
    channel: String,
    decoder: StreamDecoder,
    printer_prefix: String,
}

impl Channel {
    fn new(channel: String, decoder: StreamDecoder) -> Self {
        Self {
            channel,
            decoder,
            printer_prefix: String::new(),
        }
    }

    fn print_channel_name(&mut self, width: usize) {
        self.printer_prefix = format!("[{:width$}] ", self.channel, width = width);
    }

    async fn process(
        &mut self,
        bytes: &[u8],
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        if let Some(data) = self.decoder.process(bytes).ok().flatten() {
            self.write(&data.to_string(), shared_writer, copy_to).await;
        }
    }

    async fn flush(
        &mut self,
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        let data = self.decoder.flush();
        if !data.is_empty() {
            self.write(&data, shared_writer, copy_to).await;
        }
    }

    async fn write(
        &self,
        data: &str,
        shared_writer: &impl AsyncFn(&str),
        copy_to: Option<&mut tokio::fs::File>,
    ) {
        let message = format!("{}{}", self.printer_prefix, data);
        shared_writer(&message).await;
        if let Some(copy_to) = copy_to {
            // Silently discarding output file errors
            _ = copy_to.write_all(data.as_bytes()).await;
        }
    }
}
