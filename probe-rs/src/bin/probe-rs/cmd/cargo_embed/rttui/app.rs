use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use probe_rs::Core;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, Paragraph, Tabs},
    Terminal,
};
use std::{collections::BTreeMap, io::Write};
use std::{fmt::write, path::PathBuf, sync::mpsc::TryRecvError};

use crate::{
    cmd::cargo_embed::rttui::tab::TabData,
    util::rtt::{DataFormat, DefmtState, RttActiveTarget},
};

use super::super::config;
use super::{event::Events, tab::Tab};

use event::KeyModifiers;

/// App holds the state of the application
pub struct App<'defmt> {
    tabs: Vec<Tab<'defmt>>,
    current_tab: usize,

    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    events: Events,
    history_path: Option<PathBuf>,
    logname: String,
}

impl<'defmt> App<'defmt> {
    pub fn new(
        rtt: RttActiveTarget,
        config: config::Config,
        logname: String,
        defmt_state: Option<&'defmt DefmtState>,
    ) -> Result<Self> {
        let mut tabs = Vec::new();

        let mut up_channels = BTreeMap::new();
        let mut down_channels = BTreeMap::new();

        for channel in rtt.active_channels {
            if let Some(up) = channel.up_channel {
                up_channels.insert(up.number(), up);
            }
            if let Some(down) = channel.down_channel {
                down_channels.insert(down.number(), down);
            }
        }

        for channel in config.rtt.channels {
            tabs.push(Tab::new(
                channel.up.and_then(|up| up_channels.remove(&up)),
                channel.down.and_then(|down| down_channels.remove(&down)),
                channel.name,
                channel.format,
                channel.socket,
                defmt_state,
            ));
        }

        // Display all detected channels as String channels
        for channel in up_channels.into_values() {
            let number = channel.number();
            tabs.push(Tab::new(
                Some(channel),
                down_channels.remove(&number),
                None,
                DataFormat::String,
                None,
                defmt_state,
            ));
        }

        for channel in down_channels.into_values() {
            tabs.push(Tab::new(
                None,
                Some(channel),
                None,
                DataFormat::String,
                None,
                defmt_state,
            ));
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
        self.terminal
            .draw(|f| {
                let tab = &self.tabs[self.current_tab];

                let has_down_channel = tab.has_down_channel();

                let chunks = layout_chunks(f, has_down_channel);
                render_tabs(f, chunks[0], &self.tabs, self.current_tab);

                let input = tab.input().to_owned();
                let scroll_offset = tab.scroll_offset();

                let height = chunks[1].height as usize;

                let binary_message;
                let messages_wrapped = match tab.data() {
                    TabData::Strings { messages, .. } => {
                        // We need to collect to generate message_num :(
                        messages
                            .iter()
                            .flat_map(|m| textwrap::wrap(m, chunks[1].width as usize))
                            .collect()
                    }
                    TabData::Binary { data } => {
                        // probably pretty bad
                        binary_message = data.iter().fold(
                            String::with_capacity(data.len() * 6),
                            |mut output, byte| {
                                let _ = write(&mut output, format_args!("{byte:#04x}, "));
                                output
                            },
                        );
                        textwrap::wrap(&binary_message, chunks[1].width as usize)
                    }
                };

                let message_num = messages_wrapped.len();

                let messages = messages_wrapped
                    .into_iter()
                    .skip(message_num - (height + scroll_offset).min(message_num))
                    .take(height);

                let messages = List::new(messages).block(Block::default().borders(Borders::NONE));
                f.render_widget(messages, chunks[1]);

                if has_down_channel {
                    let input = Paragraph::new(input.as_str())
                        .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
                    f.render_widget(input, chunks[2]);
                }

                if message_num < height + scroll_offset {
                    self.tabs[self.current_tab]
                        .set_scroll_offset(message_num - height.min(message_num));
                }
            })
            .unwrap();
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

        // Ignore key release events emitted by Crossterm on Windows
        if event.kind != KeyEventKind::Press {
            return false;
        }

        match event.code {
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                clean_up_terminal();
                let _ = self.terminal.show_cursor();

                let Some(path) = &self.history_path else {
                    return true;
                };

                for (i, tab) in self.tabs.iter().enumerate() {
                    let extension = match tab.data() {
                        TabData::Strings { .. } => "txt",
                        TabData::Binary { .. } => "dat",
                    };
                    let name = format!("{}_channel{i}.{extension}", self.logname);
                    let sanitize_options = sanitize_filename::Options {
                        replacement: "_",
                        ..Default::default()
                    };
                    let sanitized_name =
                        sanitize_filename::sanitize_with_options(name, sanitize_options);
                    let final_path = path.join(sanitized_name);

                    match tab.data() {
                        TabData::Strings { messages } => {
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
                            for line in messages {
                                if let Err(e) = writeln!(file, "{line}") {
                                    eprintln!("\nError writing log channel {i}: {e}");
                                    break;
                                }
                            }
                            // Flush file
                            if let Err(e) = file.flush() {
                                eprintln!("Error writing log channel {i}: {e}")
                            }
                        }

                        TabData::Binary { data, .. } => {
                            if let Err(e) = std::fs::write(final_path, data) {
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
            KeyCode::Enter => self.push_rtt(core),
            KeyCode::Char(c) => self.current_tab_mut().append_char(c),
            KeyCode::Backspace => self.current_tab_mut().pop_char(),
            KeyCode::PageUp => self.current_tab_mut().scroll_up(),
            KeyCode::PageDown => self.current_tab_mut().scroll_down(),
            _ => {}
        }

        false
    }

    pub fn current_tab_mut(&mut self) -> &mut Tab<'defmt> {
        &mut self.tabs[self.current_tab]
    }

    /// Polls the RTT target for new data on all channels.
    ///
    /// # Errors
    /// If formatting a timestamp fails,
    /// this function will abort and return a [`time::Error`].
    pub fn poll_rtt(&mut self, core: &mut Core) -> Result<(), time::Error> {
        for channel in self.tabs.iter_mut() {
            channel.poll_rtt(core)?;
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
    tabs: &[Tab<'_>],
    current_tab: usize,
) {
    let tab_names = tabs.iter().map(|t| t.name());
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
