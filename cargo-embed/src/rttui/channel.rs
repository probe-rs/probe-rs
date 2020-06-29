use std::fmt;

use chrono::Local;
use probe_rs_rtt::{DownChannel, UpChannel};

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct ChannelConfig {
    pub up: Option<usize>,
    pub down: Option<usize>,
    pub name: Option<String>,
}

#[derive(Debug)]
pub struct ChannelState {
    up_channel: Option<UpChannel>,
    down_channel: Option<DownChannel>,
    name: String,
    messages: Vec<String>,
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
    ) -> Self {
        let name = name
            .clone()
            .or(up_channel.as_ref().and_then(|up| up.name().map(Into::into)))
            .or(down_channel
                .as_ref()
                .and_then(|down| down.name().map(Into::into)))
            .unwrap_or("Unnamed channel".to_owned());

        Self {
            up_channel,
            down_channel,
            name,
            messages: Vec::new(),
            last_line_done: true,
            input: String::new(),
            scroll_offset: 0,
            rtt_buffer: RttBuffer([0u8; 1024]),
            show_timestamps,
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

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    /// Polls the RTT target for new data on the specified channel.
    ///
    /// Processes all the new data and adds it to the linebuffer of the respective channel.
    pub fn poll_rtt(&mut self) {
        let now = Local::now();

        // TODO: Proper error handling.
        let count = if let Some(channel) = self.up_channel.as_mut() {
            match channel.read(self.rtt_buffer.0.as_mut()) {
                Ok(count) => count,
                Err(err) => {
                    eprintln!("\nError reading from RTT: {}", err);
                    return;
                }
            }
        } else {
            0
        };

        if count == 0 {
            return;
        }

        // First, convert the incoming bytes to UTF8.
        let mut incoming = String::from_utf8_lossy(&self.rtt_buffer.0[..count]).to_string();

        // Then pop the last stored line from our line buffer if possible and append our new line.
        let last_line_done = self.last_line_done;
        if !last_line_done {
            if let Some(last_line) = self.messages.pop() {
                incoming = last_line + &incoming;
            }
        }
        self.last_line_done = incoming.chars().last().unwrap() == '\n';

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
