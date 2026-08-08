use std::{
    cell::{Ref, RefCell},
    fmt::write,
    rc::Rc,
};

use crate::cmd::cargo_embed::rttui::channel::ChannelData;

use super::channel::UpChannel;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabConfig {
    /// Which up channel to use.
    pub up_channel: u32,

    /// Which down channel to use, if any.
    #[serde(default)]
    pub down_channel: Option<u32>,

    /// The name of the tab. If not set, the name of the up channel is used.
    #[serde(default)]
    pub name: Option<String>,

    /// Whether to hide the tab. By default, all up channels are shown in separate tabs.
    #[serde(default)]
    pub hide: bool,
}

pub struct Tab {
    up_channel: Rc<RefCell<UpChannel>>,
    down_channel: Option<DownChannel>,
    name: String,
    scroll_offset: usize,
    messages: Vec<String>,
    last_processed: usize,
    last_width: usize,
}

/// Editable input plus bytes waiting for the target to accept them.
struct DownChannel {
    number: u32,
    /// Line the user is typing.
    input: Vec<u8>,
    /// Outbound bytes not yet accepted by the target (includes newlines).
    pending: Vec<u8>,
    /// True while an RPC write of the current pending prefix is in flight.
    flush_in_flight: bool,
    /// True after a flush left bytes in `pending` (target did not accept everything).
    blocked: bool,
}

impl Tab {
    pub fn new(
        up_channel: Rc<RefCell<UpChannel>>,
        down_channel: Option<u32>,
        name: Option<String>,
    ) -> Self {
        Self {
            name: name.unwrap_or_else(|| up_channel.borrow().channel_name().to_string()),
            up_channel,
            down_channel: down_channel.map(|number| DownChannel {
                number,
                input: Vec::new(),
                pending: Vec::new(),
                flush_in_flight: false,
                blocked: false,
            }),
            scroll_offset: 0,
            messages: Vec::new(),
            last_processed: 0,
            last_width: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    pub fn up_channel(&self) -> Ref<'_, UpChannel> {
        self.up_channel.borrow()
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.set_scroll_offset(
            self.scroll_offset
                .saturating_add(lines)
                .min(self.messages.len()),
        );
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.set_scroll_offset(self.scroll_offset.saturating_sub(lines));
    }

    pub fn clear(&mut self) {
        self.set_scroll_offset(0);
    }

    pub fn has_down_channel(&self) -> bool {
        self.down_channel.is_some()
    }

    /// True when the target has not accepted all queued outbound bytes.
    pub fn is_blocked(&self) -> bool {
        self.down_channel.as_ref().is_some_and(|down| down.blocked)
    }

    /// True when this tab has pending bytes and is not already flushing.
    pub fn can_start_flush(&self) -> bool {
        self.down_channel
            .as_ref()
            .is_some_and(|down| !down.flush_in_flight && !down.pending.is_empty())
    }

    pub fn push_input(&mut self, c: char) {
        if let Some(down) = self.down_channel.as_mut() {
            let mut buf = [0u8; 4];
            down.input
                .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }

    pub fn pop_input(&mut self) {
        let Some(down) = self.down_channel.as_mut() else {
            return;
        };

        // Remove the last UTF-8 character.
        while let Some(byte) = down.input.pop() {
            if byte & 0xC0 != 0x80 {
                break;
            }
        }
    }

    /// Text for the down-channel input bar: pending outbound bytes, then the
    /// line currently being typed. Newlines in pending are shown as `↵`.
    pub fn down_line_display(&self) -> Option<String> {
        let down = self.down_channel.as_ref()?;
        let mut text = String::with_capacity(down.pending.len() + down.input.len());
        if !down.pending.is_empty() {
            text.push_str(&String::from_utf8_lossy(&down.pending).replace('\n', "↵"));
        }
        text.push_str(std::str::from_utf8(&down.input).expect("down-channel input is UTF-8"));
        Some(text)
    }

    /// Move the current input line into the pending outbound buffer and append a newline.
    pub fn queue_input_line(&mut self) {
        let Some(down) = self.down_channel.as_mut() else {
            return;
        };
        if down.pending.is_empty() {
            down.pending.extend_from_slice(&down.input);
            down.pending.push(b'\n');
            down.input.clear();
        }
    }

    /// Snapshot pending bytes for an RPC write. Pending is kept until the response.
    pub fn begin_flush(&mut self) -> Option<(u32, Vec<u8>)> {
        let down = self.down_channel.as_mut()?;
        if down.flush_in_flight || down.pending.is_empty() {
            return None;
        }
        down.flush_in_flight = true;
        Some((down.number, down.pending.clone()))
    }

    /// Consume bytes accepted by the target from the front of `pending`.
    pub fn finish_flush(&mut self, written: usize) {
        let Some(down) = self.down_channel.as_mut() else {
            return;
        };
        down.flush_in_flight = false;
        let written = written.min(down.pending.len());
        down.pending.drain(..written);
        down.blocked = !down.pending.is_empty();
    }

    /// Keep pending unchanged after a failed write and mark the channel blocked.
    pub fn abort_flush(&mut self) {
        let Some(down) = self.down_channel.as_mut() else {
            return;
        };
        down.flush_in_flight = false;
        down.blocked = !down.pending.is_empty();
    }

    pub fn update_messages(&mut self, width: usize, height: usize) {
        if self.last_width != width {
            // If the width changes, we need to reprocess all messages.
            self.last_width = width;
            self.last_processed = 0;
            self.set_scroll_offset(0);
            self.messages.clear();
        }

        let old_message_count = self.messages.len();
        match &self.up_channel.borrow().data {
            ChannelData::Strings { messages, .. } => {
                // We strip ANSI sequences because they interfere with text wrapping.
                //  - It's not obvious how we could tell defmt_parser to not emit ANSI sequences.
                //  - Calling textwrap on a string with ANSI sequences may break a sequence into
                // multiple lines, which is incorrect.
                //  - We can only interpret the sequences by emitting ratatui span styles, but at
                // that point we can no longer wrap the text using textwrap.
                //  - Leaving sequences in the output intact is just a bad experience.

                for line in messages.iter().skip(self.last_processed).map(strip_ansi) {
                    // TODO: we shouldn't assume that one message is one complete line. If the
                    // last line did not end with a newline, we should append to that line instead.

                    // Trim a single newline from the end
                    let line = if line.ends_with('\n') {
                        &line[..line.len() - 1]
                    } else {
                        &line
                    };

                    self.messages
                        .extend(textwrap::wrap(line, width).into_iter().map(String::from));
                }

                self.last_processed = messages.len();
            }
            ChannelData::Binary { data } => {
                let mut string = self.messages.pop().unwrap_or_default();

                if !data.is_empty() {
                    // 4 characters per byte (0xAB) + 1 space, except at the end
                    string.reserve(data.len() * 5 - 1);
                }

                let string =
                    data.iter()
                        .skip(self.last_processed)
                        .fold(string, |mut output, byte| {
                            if !output.is_empty() {
                                output.push(' ');
                            }
                            let _ = write(&mut output, format_args!("{byte:#04x}"));
                            output
                        });

                self.messages
                    .extend(textwrap::wrap(&string, width).into_iter().map(String::from));
                self.last_processed = data.len();
            }
        };

        let inserted = self.messages.len() - old_message_count;

        // Move scroll offset if we're not at the bottom
        if self.scroll_offset != 0 {
            // This scroll ensures that inserting new messages will not move our view.
            self.scroll_up(inserted);

            // Don't let scrolling up more than necessary to show all messages.
            // Doing so would require the user to scroll down more times than necessary.
            self.set_scroll_offset(
                self.scroll_offset
                    .min(self.messages.len().saturating_sub(height)),
            );
        }
    }

    pub fn messages(&self, height: usize) -> impl Iterator<Item = &str> + '_ {
        let message_num = self.messages.len();
        self.messages
            .iter()
            .map(|s| s.as_str())
            .skip(message_num.saturating_sub(height + self.scroll_offset))
            .take(height)
    }
}

/// Removes ANSI escape sequences from a string.
fn strip_ansi(s: impl AsRef<str>) -> String {
    fn text_block(output: ansi_parser::Output<'_>) -> Option<&str> {
        match output {
            ansi_parser::Output::TextBlock(text) => Some(text),
            _ => None,
        }
    }

    // TODO: use a cow: if ansi-parser returns a single string, do not allocate
    use ansi_parser::AnsiParser;
    s.as_ref()
        .ansi_parse()
        .filter_map(text_block)
        .collect::<String>()
}
