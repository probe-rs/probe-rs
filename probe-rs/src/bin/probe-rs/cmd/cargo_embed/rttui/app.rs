use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use probe_rs::Core;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs},
    Terminal,
};
use std::io::Write;
use std::{fmt::write, path::PathBuf, sync::mpsc::TryRecvError};

use crate::util::rtt::DataFormat;

use super::{
    super::{config, DefmtInformation},
    channel::ChannelData,
};

use super::{channel::ChannelState, event::Events};

use event::KeyModifiers;

/// App holds the state of the application
pub struct App<'defmt> {
    tabs: Vec<ChannelState<'defmt>>,
    current_tab: usize,

    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    events: Events,
    history_path: Option<PathBuf>,
    logname: String,
}

impl<'defmt> App<'defmt> {
    pub fn new(
        mut rtt: probe_rs::rtt::Rtt,
        config: &config::Config,
        logname: String,
        defmt_state: Option<&'defmt DefmtInformation>,
    ) -> Result<Self> {
        let mut tabs = Vec::new();
        if !config.rtt.channels.is_empty() {
            for channel in &config.rtt.channels {
                let data = match channel.format {
                    DataFormat::String => ChannelData::new_string(config.rtt.show_timestamps),
                    DataFormat::BinaryLE => ChannelData::new_binary(),
                    DataFormat::Defmt => {
                        let defmt_information = defmt_state.ok_or_else(|| {
                            anyhow!("Defmt information required for defmt channel {:?}", channel)
                        })?;
                        let stream_decoder = defmt_information.table.new_stream_decoder();

                        ChannelData::new_defmt(stream_decoder, defmt_information)
                    }
                };

                tabs.push(ChannelState::new(
                    channel.up.and_then(|up| rtt.up_channels.take(up)),
                    channel.down.and_then(|down| rtt.down_channels.take(down)),
                    channel.name.clone(),
                    data,
                    channel.socket,
                ))
            }
        } else {
            // Display all detected channels as String channels
            for channel in rtt.up_channels.into_iter() {
                let number = channel.number();
                tabs.push(ChannelState::new(
                    Some(channel),
                    rtt.down_channels.take(number),
                    None,
                    ChannelData::new_string(config.rtt.show_timestamps),
                    None,
                ));
            }

            for channel in rtt.down_channels.into_iter() {
                tabs.push(ChannelState::new(
                    None,
                    Some(channel),
                    None,
                    ChannelData::new_string(config.rtt.show_timestamps),
                    None,
                ));
            }
        }

        // Code farther down relies on tabs being configured and might panic
        // otherwise.
        if tabs.is_empty() {
            return Err(anyhow!(
                "Failed to initialize RTT UI: No RTT channels configured"
            ));
        }

        let events = Events::new();

        enable_raw_mode().context("Failed to enable 'raw' mode for terminal")?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen).unwrap();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.hide_cursor();

        let history_path = {
            if !config.rtt.log_enabled {
                None
            } else {
                //when is the right time if ever to fail if the directory or file cant be created?
                //should we create the path on startup or when we write
                match std::fs::create_dir_all(&config.rtt.log_path) {
                    Ok(_) => Some(config.rtt.log_path.clone()),
                    Err(_) => {
                        tracing::warn!("Could not create log directory");
                        None
                    }
                }
            }
        };

        Ok(Self {
            tabs,
            current_tab: 0,
            terminal,
            events,
            history_path,
            logname,
        })
    }

    pub fn render(&mut self) {
        let input = self.current_tab().input().to_owned();
        let has_down_channel = self.current_tab().has_down_channel();
        let scroll_offset = self.current_tab().scroll_offset();

        let tabs = &self.tabs;
        let current_tab = self.current_tab;
        let mut height = 0;
        let mut messages_wrapped: Vec<String> = Vec::new();

        self.terminal
            .draw(|f| {
                let chunks = layout_chunks(f, has_down_channel);
                render_tabs(f, chunks[0], tabs, current_tab);

                height = chunks[1].height as usize;
                match tabs[current_tab].data() {
                    ChannelData::String { data: messages, .. } => {
                        // We need to collect to generate message_num :(
                        messages_wrapped = messages
                            .iter()
                            .flat_map(|m| textwrap::wrap(m, chunks[1].width as usize))
                            .map(|s| s.into_owned())
                            .collect();
                    }
                    ChannelData::Binary { data } => {
                        // probably pretty bad
                        messages_wrapped.push(data.iter().fold(
                            String::new(),
                            |mut output, byte| {
                                let _ = write(&mut output, format_args!("{byte:#04x}, "));
                                output
                            },
                        ));
                    }

                    ChannelData::Defmt { messages, .. } => {
                        messages_wrapped.extend_from_slice(messages);
                    }
                };

                let message_num = messages_wrapped.len();

                let messages: Vec<ListItem> = messages_wrapped
                    .iter()
                    .skip(message_num - (height + scroll_offset).min(message_num))
                    .take(height)
                    .map(|s| ListItem::new(vec![Line::from(Span::raw(s))]))
                    .collect();

                let messages = List::new(messages).block(Block::default().borders(Borders::NONE));
                f.render_widget(messages, chunks[1]);

                if has_down_channel {
                    let input = Paragraph::new(Line::from(vec![Span::raw(input.clone())]))
                        .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
                    f.render_widget(input, chunks[2]);
                }
            })
            .unwrap();

        let message_num = messages_wrapped.len();
        let scroll_offset = self.tabs[self.current_tab].scroll_offset();
        if message_num < height + scroll_offset {
            self.current_tab_mut()
                .set_scroll_offset(message_num - height.min(message_num));
        }
    }

    /// Returns true if the application should exit.
    pub fn handle_event(&mut self, core: &mut Core) -> bool {
        let event = match self.events.next() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                tracing::warn!(
                    "Unable to receive anymore input events from terminal, shutting down."
                );
                return true;
            }
        };

        match event.code {
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                clean_up_terminal();
                let _ = self.terminal.show_cursor();

                let Some(path) = &self.history_path else {
                    return true;
                };

                for (i, tab) in self.tabs.iter().enumerate() {
                    match tab.data() {
                        ChannelData::Defmt { .. } => {
                            eprintln!(
                                "Not saving tab {} as saving defmt logs is currently unsupported.",
                                i + 1
                            );
                            continue;
                        }
                        ChannelData::String { data, .. } => {
                            let extension = "txt";
                            let name = format!("{}_channel{}.{}", self.logname, i, extension);
                            let sanitize_options = sanitize_filename::Options {
                                replacement: "_",
                                ..Default::default()
                            };
                            let sanitized_name =
                                sanitize_filename::sanitize_with_options(name, sanitize_options);
                            let final_path = path.join(sanitized_name);
                            let mut file = match std::fs::File::create(&final_path) {
                                Ok(file) => file,
                                Err(e) => {
                                    eprintln!(
                                        "\nCould not create log file {}: {}",
                                        final_path.display(),
                                        e
                                    );
                                    continue;
                                }
                            };
                            for line in data {
                                if let Err(e) = writeln!(file, "{line}") {
                                    eprintln!("\nError writing log channel {i}: {e}");
                                    continue;
                                }
                            }
                            // Flush file
                            if let Err(e) = file.flush() {
                                eprintln!("Error writing log channel {i}: {e}")
                            }
                        }

                        ChannelData::Binary { data, .. } => {
                            let extension = "dat";
                            let name = format!("{}_channel{}.{}", self.logname, i, extension);
                            let sanitize_options = sanitize_filename::Options {
                                replacement: "_",
                                ..Default::default()
                            };
                            let sanitized_name =
                                sanitize_filename::sanitize_with_options(name, sanitize_options);
                            let final_path = path.join(sanitized_name);
                            let mut file = match std::fs::File::create(&final_path) {
                                Ok(file) => file,
                                Err(e) => {
                                    eprintln!(
                                        "\nCould not create log file {}: {}",
                                        final_path.display(),
                                        e
                                    );
                                    continue;
                                }
                            };
                            match file.write(data) {
                                Ok(_) => {}
                                Err(e) => {
                                    eprintln!("\nError writing log channel {i}: {e}");
                                    continue;
                                }
                            }
                            // Flush file
                            if let Err(e) = file.flush() {
                                eprintln!("Error writing log channel {i}: {e}")
                            }
                        }
                    }
                }

                return true;
            }
            KeyCode::Char('l') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current_tab_mut().clear();
            }
            KeyCode::F(n) => {
                let n = n as usize - 1;
                if n < self.tabs.len() {
                    self.current_tab = n;
                }
            }
            KeyCode::Enter => {
                self.push_rtt(core);
            }
            KeyCode::Char(c) => {
                self.current_tab_mut().input_mut().push(c);
            }
            KeyCode::Backspace => {
                self.current_tab_mut().input_mut().pop();
            }
            KeyCode::PageUp => {
                self.current_tab_mut().scroll_up();
            }
            KeyCode::PageDown => {
                self.current_tab_mut().scroll_down();
            }
            _ => {}
        }

        false
    }

    pub fn current_tab(&self) -> &ChannelState<'defmt> {
        &self.tabs[self.current_tab]
    }

    pub fn current_tab_mut(&mut self) -> &mut ChannelState<'defmt> {
        &mut self.tabs[self.current_tab]
    }

    /// Polls the RTT target for new data on all channels.
    ///
    /// # Errors
    /// If formatting a timestamp fails,
    /// this function will abort and return a [`time::Error`].
    pub fn poll_rtt(
        &mut self,
        core: &mut Core,
        offset: time::UtcOffset,
    ) -> Result<(), time::Error> {
        for channel in self.tabs.iter_mut() {
            channel.poll_rtt(core, offset)?;
        }

        Ok(())
    }

    pub fn push_rtt(&mut self, core: &mut Core) {
        self.tabs[self.current_tab].push_rtt(core);
    }
}

fn layout_chunks(
    f: &mut ratatui::Frame,
    has_down_channel: bool,
) -> std::rc::Rc<[ratatui::prelude::Rect]> {
    let constraints = if has_down_channel {
        &[
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ][..]
    } else {
        &[Constraint::Length(1), Constraint::Min(1)][..]
    };
    Layout::default()
        .direction(Direction::Vertical)
        .margin(0)
        .constraints(constraints)
        .split(f.size())
}

fn render_tabs(
    f: &mut ratatui::Frame,
    chunk: ratatui::prelude::Rect,
    tabs: &[ChannelState<'_>],
    current_tab: usize,
) {
    let tab_names = tabs
        .iter()
        .map(|t| Line::from(t.name()))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(tab_names)
        .select(current_tab)
        .style(Style::default().fg(Color::Black).bg(Color::Yellow))
        .highlight_style(
            Style::default()
                .fg(Color::Green)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunk);
}

pub fn clean_up_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}
