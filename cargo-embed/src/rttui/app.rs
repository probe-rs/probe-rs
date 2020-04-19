use probe_rs_rtt::RttChannel;
use std::io::{Read, Seek, Write};
use termion::{
    cursor::Goto,
    event::Key,
    raw::{IntoRawMode, RawTerminal},
    screen::AlternateScreen,
};
use tui::{
    backend::TermionBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, Paragraph, Tabs, Text},
    Terminal,
};
use unicode_width::UnicodeWidthStr;

use super::channel::ChannelState;
use super::event::{Event, Events};

use crate::config::CONFIG;

/// App holds the state of the application
pub struct App {
    tabs: Vec<ChannelState>,
    current_tab: usize,

    terminal: Terminal<TermionBackend<AlternateScreen<RawTerminal<std::io::Stdout>>>>,
    events: Events,
}

fn pull_channel<C: RttChannel>(channels: &mut Vec<C>, n: usize) -> Option<C> {
    let c =
        channels
            .iter()
            .enumerate()
            .find_map(|(i, c)| if c.number() == n { Some(i) } else { None });

    c.map(|c| channels.remove(c))
}

impl App {
    pub fn new(mut rtt: probe_rs_rtt::Rtt) -> Self {
        let stdout = std::io::stdout().into_raw_mode().unwrap();
        let stdout = AlternateScreen::from(stdout);
        let backend = TermionBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        let events = Events::new();

        let mut tabs = Vec::new();
        let mut up_channels = rtt.up_channels().drain().collect::<Vec<_>>();
        let mut down_channels = rtt.down_channels().drain().collect::<Vec<_>>();
        if !CONFIG.rtt.channels.is_empty() {
            for channel in &CONFIG.rtt.channels {
                tabs.push(ChannelState::new(
                    channel.up.and_then(|up| pull_channel(&mut up_channels, up)),
                    channel
                        .down
                        .and_then(|down| pull_channel(&mut down_channels, down)),
                    channel.name.clone(),
                ))
            }
        } else {
            for channel in up_channels.into_iter() {
                let number = channel.number();
                tabs.push(ChannelState::new(
                    Some(channel),
                    pull_channel(&mut down_channels, number),
                    None,
                ));
            }

            for channel in down_channels {
                tabs.push(ChannelState::new(None, Some(channel), None));
            }
        }

        Self {
            tabs,
            current_tab: 0,

            terminal,
            events,
        }
    }

    pub fn get_rtt_symbol<'b, T: Read + Seek>(file: &'b mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if let Ok(_) = file.read_to_end(&mut buffer) {
            if let Ok(binary) = goblin::elf::Elf::parse(&buffer.as_slice()) {
                for sym in &binary.syms {
                    if let Some(Ok(name)) = binary.strtab.get(sym.st_name) {
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

    pub fn render(&mut self) {
        let input = self.current_tab().input().to_owned();
        let has_down_channel = self.current_tab().has_down_channel();
        let scroll_offset = self.current_tab().scroll_offset();
        let message_num = self.current_tab().messages().len();
        let messages = self.current_tab().messages().clone();
        let tabs = &self.tabs;
        let current_tab = self.current_tab;
        let mut height = 0;
        self.terminal
            .draw(|mut f| {
                let constraints = if has_down_channel {
                    &[
                        Constraint::Length(1),
                        Constraint::Min(1),
                        Constraint::Length(1),
                    ][..]
                } else {
                    &[Constraint::Length(1), Constraint::Min(1)][..]
                };
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(0)
                    .constraints(constraints)
                    .split(f.size());

                let tab_names = tabs.iter().map(|t| t.name()).collect::<Vec<_>>();
                let mut tabs = Tabs::default()
                    .titles(&tab_names.as_slice())
                    .select(current_tab)
                    .style(Style::default().fg(Color::Black).bg(Color::Yellow))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Green)
                            .bg(Color::Yellow)
                            .modifier(Modifier::BOLD),
                    );
                f.render(&mut tabs, chunks[0]);

                height = chunks[1].height as usize;

                let messages = messages
                    .iter()
                    .map(|m| Text::raw(m))
                    .skip(message_num - (height + scroll_offset).min(message_num))
                    .take(height);
                let mut messages =
                    List::new(messages).block(Block::default().borders(Borders::NONE));
                f.render(&mut messages, chunks[1]);

                if has_down_channel {
                    let text = [Text::raw(input.clone())];
                    let mut input = Paragraph::new(text.iter())
                        .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
                    f.render(&mut input, chunks[2]);
                }
            })
            .unwrap();

        let message_num = self.tabs[self.current_tab].messages().len();
        let scroll_offset = self.tabs[self.current_tab].scroll_offset();
        if message_num < height + scroll_offset {
            self.current_tab_mut()
                .set_scroll_offset(message_num - height.min(message_num));
        }

        if has_down_channel {
            // Put the cursor back inside the input box
            let height = self.terminal.size().map(|s| s.height).unwrap_or(1);
            write!(
                self.terminal.backend_mut(),
                "{}",
                Goto(input.width() as u16 + 1, height)
            )
            .unwrap();
            // stdout is buffered, flush it to see the effect immediately when hitting backspace
            std::io::stdout().flush().ok();
        }
    }

    /// Returns true if the application should exit.
    pub fn handle_event(&mut self) -> bool {
        match self.events.next().unwrap() {
            Event::Input(input) => match input {
                Key::Ctrl('c') => true,
                Key::F(n) => {
                    let n = n as usize - 1;
                    if n < self.tabs.len() {
                        self.current_tab = n as usize;
                    }
                    false
                }
                Key::Char('\n') => {
                    self.push_rtt();
                    false
                }
                Key::Char(c) => {
                    self.current_tab_mut().input_mut().push(c);
                    false
                }
                Key::Backspace => {
                    self.current_tab_mut().input_mut().pop();
                    false
                }
                Key::PageUp => {
                    self.current_tab_mut().scroll_up();
                    false
                }
                Key::PageDown => {
                    self.current_tab_mut().scroll_down();
                    false
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub fn current_tab(&self) -> &ChannelState {
        &self.tabs[self.current_tab]
    }

    pub fn current_tab_mut(&mut self) -> &mut ChannelState {
        &mut self.tabs[self.current_tab]
    }

    /// Polls the RTT target for new data on all channels.
    pub fn poll_rtt(&mut self) {
        for channel in &mut self.tabs {
            channel.poll_rtt();
        }
    }

    pub fn push_rtt(&mut self) {
        self.tabs[self.current_tab].push_rtt();
    }
}
