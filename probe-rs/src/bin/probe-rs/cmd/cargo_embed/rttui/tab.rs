use std::collections::BTreeMap;

use probe_rs::Core;

use crate::util::rtt::RttActiveDownChannel;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabConfig {
    pub up_channel: usize,
    #[serde(default)]
    pub down_channel: Option<usize>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub hide: bool,
}

#[derive(Debug)]
pub struct Tab {
    up_channel: usize,
    down_channel: Option<(usize, String)>,
    name: String,
    scroll_offset: usize,
}

impl Tab {
    pub fn new(up_channel: usize, down_channel: Option<usize>, name: Option<String>) -> Self {
        let name = name.unwrap_or_else(|| "Unnamed channel".to_owned());

        Self {
            up_channel,
            down_channel: down_channel.map(|id| (id, String::new())),
            name,
            scroll_offset: 0,
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

    pub fn up_channel(&self) -> usize {
        self.up_channel
    }

    pub fn scroll_up(&mut self) {
        self.set_scroll_offset(self.scroll_offset().saturating_add(1));
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

    pub fn send_input(
        &mut self,
        core: &mut Core,
        channels: &mut BTreeMap<usize, RttActiveDownChannel>,
    ) -> anyhow::Result<()> {
        if let Some((channel, input)) = self.down_channel.as_mut() {
            let channel = channels.get_mut(channel).expect("down channel disappeared");
            input.push('\n');
            channel.push_rtt(core, input.as_str())?;
            input.clear();
        }

        Ok(())
    }
}
