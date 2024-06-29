use std::{
    cell::{Ref, RefCell},
    fmt::write,
    rc::Rc,
};

use probe_rs::Core;

use crate::{cmd::cargo_embed::rttui::channel::ChannelData, util::rtt::RttActiveDownChannel};

use super::channel::UpChannel;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabConfig {
    /// Which up channel to use.
    pub up_channel: usize,

    /// Which down channel to use, if any.
    #[serde(default)]
    pub down_channel: Option<usize>,

    /// The name of the tab. If not set, the name of the up channel is used.
    #[serde(default)]
    pub name: Option<String>,

    /// Whether to hide the tab. By default, all up channels are shown in separate tabs.
    #[serde(default)]
    pub hide: bool,
}

pub struct Tab {
    up_channel: Rc<RefCell<UpChannel>>,
    down_channel: Option<(Rc<RefCell<RttActiveDownChannel>>, String)>,
    name: String,
    scroll_offset: usize,
    messages: Vec<String>,
    last_processed: usize,
    last_width: usize,
}

impl Tab {
    pub fn new(
        up_channel: Rc<RefCell<UpChannel>>,
        down_channel: Option<Rc<RefCell<RttActiveDownChannel>>>,
        name: Option<String>,
    ) -> Self {
        Self {
            name: name.unwrap_or_else(|| up_channel.borrow().channel_name().to_string()),
            up_channel,
            down_channel: down_channel.map(|down| (down, String::new())),
            scroll_offset: 0,
            messages: Vec::new(),
            last_processed: 0,
            last_width: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn set_scroll_offset(&mut self, value: usize) {
        self.scroll_offset = value;
    }

    pub fn up_channel(&self) -> Ref<'_, UpChannel> {
        self.up_channel.borrow()
    }

    pub fn scroll_up(&mut self) {
        self.set_scroll_offset(
            self.scroll_offset()
                .saturating_add(1)
                .min(self.messages.len()),
        );
    }

    pub fn scroll_down(&mut self) {
        self.set_scroll_offset(self.scroll_offset().saturating_sub(1));
    }

    pub fn clear(&mut self) {
        self.set_scroll_offset(0);
    }

    pub fn push_input(&mut self, c: char) {
        if let Some((_, input)) = self.down_channel.as_mut() {
            input.push(c);
        }
    }

    pub fn pop_input(&mut self) {
        if let Some((_, input)) = self.down_channel.as_mut() {
            input.pop();
        }
    }

    pub fn input(&self) -> Option<&str> {
        self.down_channel.as_ref().map(|(_, input)| input.as_str())
    }

    pub fn send_input(&mut self, core: &mut Core) -> anyhow::Result<()> {
        if let Some((channel, input)) = self.down_channel.as_mut() {
            input.push('\n');
            channel.borrow_mut().push_rtt(core, input.as_str())?;
            input.clear();
        }

        Ok(())
    }

    pub fn update_messages(&mut self, width: usize) {
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
            self.set_scroll_offset(self.scroll_offset + inserted);
        }
    }

    pub fn messages(&self, height: usize) -> impl Iterator<Item = &str> + '_ {
        let message_num = self.messages.len();
        self.messages
            .iter()
            .map(|s| s.as_str())
            .skip(message_num - (height + self.scroll_offset).min(message_num))
            .take(height)
    }
}

/// Removes ANSI escape sequences from a string.
fn strip_ansi(s: impl AsRef<str>) -> String {
    fn text_block(output: ansi_parser::Output) -> Option<&str> {
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
