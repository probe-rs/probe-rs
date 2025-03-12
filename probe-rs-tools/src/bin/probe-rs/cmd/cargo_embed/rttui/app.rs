use anyhow::{Context, Result, anyhow};
use probe_rs::Core;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        event::{self, KeyCode, KeyEventKind},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, Paragraph, Tabs},
};
use std::{cell::RefCell, io::Write, rc::Rc};
use std::{path::PathBuf, sync::mpsc::TryRecvError};
use time::UtcOffset;

use crate::{
    cmd::cargo_embed::rttui::{channel::ChannelData, tab::TabConfig},
    util::rtt::{DataFormat, DefmtProcessor, DefmtState, RttDecoder, client::ConfiguredRttClient},
};

use super::super::config;
use super::channel::UpChannel;
use super::{event::Events, tab::Tab};

use event::KeyModifiers;

/// App holds the state of the application
pub struct App {
    tabs: Vec<Tab>,
    current_tab: usize,

    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    events: Events,
    history_path: Option<PathBuf>,
    logname: String,

    current_height: usize,

    up_channels: Vec<Rc<RefCell<UpChannel>>>,

    client: ConfiguredRttClient,
}

impl App {
    pub fn new(
        client: ConfiguredRttClient,
        elf: Option<Vec<u8>>,
        config: config::Config,
        timestamp_offset: UtcOffset,
        logname: String,
    ) -> Result<Self> {
        let defmt_data = if let Some(elf) = elf {
            DefmtState::try_from_bytes(&elf)?
        } else {
            None
        };

        let mut tab_config = config.rtt.tabs;

        // Create channel states
        let mut up_channels = Vec::new();
        let mut down_channels = Vec::new();

        // Create tab config based on detected channels
        for up in client.up_channels() {
            let number = up.number();

            // Create a default tab config if the user didn't specify one
            if !tab_config.iter().any(|tab| tab.up_channel == number) {
                tab_config.push(TabConfig {
                    up_channel: number,
                    down_channel: None,
                    name: Some(up.channel_name()),
                    hide: false,
                });
            }

            let mut channel_config = client
                .config
                .channel_config(number)
                .cloned()
                .unwrap_or_default();

            if up.channel_name() == "defmt" {
                channel_config.data_format = DataFormat::Defmt;
            }

            // Where `channel_config` is unspecified, apply default from `default_channel_config`.
            // Is a TCP publish address configured?
            let stream = config
                .rtt
                .up_channels
                .iter()
                .find(|up_config| up_config.channel == number)
                .and_then(|up_config| up_config.socket);

            let data_format = match channel_config.data_format {
                DataFormat::String => RttDecoder::String {
                    timestamp_offset: Some(timestamp_offset),
                    last_line_done: false,
                },
                DataFormat::BinaryLE => RttDecoder::BinaryLE,
                DataFormat::Defmt if defmt_data.is_none() => {
                    tracing::warn!("Defmt data not found in ELF file");
                    continue;
                }
                DataFormat::Defmt => RttDecoder::Defmt {
                    processor: DefmtProcessor::new(
                        defmt_data.clone().unwrap(),
                        channel_config.show_timestamps,
                        channel_config.show_location,
                        channel_config.log_format.as_deref(),
                    ),
                },
            };

            up_channels.push(Rc::new(RefCell::new(UpChannel::new(
                up,
                data_format,
                stream,
            ))));
        }

        for down in client.down_channels() {
            let number = down.number();
            if !tab_config
                .iter()
                .any(|tab| tab.down_channel == Some(number))
            {
                tab_config.push(TabConfig {
                    up_channel: if up_channels.len() as u32 > number {
                        number
                    } else {
                        0
                    },
                    down_channel: Some(number),
                    name: Some(down.channel_name()),
                    hide: false,
                });
            }

            down_channels.push(Rc::new(RefCell::new(down)));
        }

        // Create tabs
        let mut tabs = Vec::new();
        for tab in tab_config {
            if tab.hide {
                continue;
            }
            let Some(up_channel) = up_channels.get(tab.up_channel as usize) else {
                tracing::warn!(
                    "Configured up channel {} does not exist, skipping tab",
                    tab.up_channel
                );
                continue;
            };

            tabs.push(Tab::new(up_channel.clone(), tab.down_channel, tab.name));
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

        let history_path = if config.rtt.log_enabled {
            //when is the right time if ever to fail if the directory or file cant be created?
            //should we create the path on startup or when we write
            match std::fs::create_dir_all(&config.rtt.log_path) {
                Ok(_) => Some(config.rtt.log_path),
                Err(error) => {
                    tracing::warn!("Could not create log directory: {error}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            tabs,
            current_tab: 0,
            terminal,
            events,
            history_path,
            logname,
            current_height: 0,

            up_channels,
            client,
        })
    }

    pub fn render(&mut self) {
        self.terminal
            .draw(|f| {
                let tab = &self.tabs[self.current_tab];

                let chunks = layout_chunks(f, tab.input().is_some());
                render_tabs(f, chunks[0], &self.tabs, self.current_tab);

                let height = chunks[1].height as usize;
                let width = chunks[1].width as usize;

                let current_tab = &mut self.tabs[self.current_tab];
                current_tab.update_messages(width, height);

                self.current_height = height;

                let messages = List::new(current_tab.messages(height))
                    .block(Block::default().borders(Borders::NONE));
                f.render_widget(messages, chunks[1]);

                if let Some(input) = current_tab.input() {
                    let input = Paragraph::new(input)
                        .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
                    f.render_widget(input, chunks[2]);
                }
            })
            .expect("Failed to render terminal UI");
    }

    /// Returns `true` if the application should exit.
    pub fn handle_event(&mut self, core: &mut Core) -> bool {
        let event = match self.events.next() {
            // Ignore key release events emitted by Crossterm on Windows
            Ok(event) if event.kind != KeyEventKind::Press => return false,
            Ok(event) => event,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                tracing::warn!("Unable to receive more input events from terminal, shutting down.");
                return true;
            }
        };

        let height = self.current_height / 2;
        let has_control = event.modifiers.contains(KeyModifiers::CONTROL);

        match event.code {
            KeyCode::Char('c') if has_control => return true,
            KeyCode::Char('l') if has_control => {
                self.current_tab_mut().clear();
            }
            KeyCode::F(n) => self.select_tab(n as usize - 1),
            KeyCode::Tab => self.next_tab(),
            KeyCode::BackTab => self.previous_tab(),
            KeyCode::Enter => self.push_rtt(core),
            KeyCode::Char(c) => {
                if has_control {
                    if let Some(digit) = c.to_digit(10).and_then(|d| d.checked_sub(1)) {
                        self.select_tab(digit as usize);
                        return false;
                    }
                }

                self.current_tab_mut().push_input(c)
            }
            KeyCode::Backspace => self.current_tab_mut().pop_input(),
            KeyCode::Up => self.current_tab_mut().scroll_up(1),
            KeyCode::Down => self.current_tab_mut().scroll_down(1),
            KeyCode::PageUp => self.current_tab_mut().scroll_up(height),
            KeyCode::PageDown => self.current_tab_mut().scroll_down(height),
            _ => {}
        }

        false
    }

    pub fn current_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.current_tab]
    }

    /// Polls the RTT target for new data on all channels.
    pub fn poll_rtt(&mut self, core: &mut Core) -> Result<()> {
        for channel in self.up_channels.iter_mut() {
            channel.borrow_mut().poll_rtt(core, &mut self.client)?;
        }

        Ok(())
    }

    pub fn push_rtt(&mut self, core: &mut Core) {
        if let Err(error) = self.tabs[self.current_tab].send_input(core, &mut self.client) {
            tracing::warn!("Failed to send input to RTT channel: {error:?}");
        }
    }

    pub(crate) fn clean_up(&mut self, core: &mut Core) -> Result<()> {
        clean_up_terminal();
        let _ = self.terminal.show_cursor();

        for (i, tab) in self.tabs.iter().enumerate() {
            self.save_tab_logs(i, tab);
        }

        self.client.clean_up(core)?;

        Ok(())
    }

    fn save_tab_logs(&self, i: usize, tab: &Tab) {
        let Some(path) = &self.history_path else {
            return;
        };

        let up_channel = tab.up_channel();

        let extension = match up_channel.data {
            ChannelData::Strings { .. } => "txt",
            ChannelData::Binary { .. } => "dat",
        };
        let name = format!("{}_channel{i}.{extension}", self.logname);
        let sanitize_options = sanitize_filename::Options {
            replacement: "_",
            ..Default::default()
        };
        let sanitized_name = sanitize_filename::sanitize_with_options(name, sanitize_options);
        let final_path = path.join(sanitized_name);

        match &up_channel.data {
            ChannelData::Strings { messages } => {
                let mut file = match std::fs::File::create(&final_path) {
                    Ok(file) => file,
                    Err(e) => {
                        eprintln!(
                            "\nCould not create log file {}: {}",
                            final_path.display(),
                            e
                        );
                        return;
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

            ChannelData::Binary { data } => {
                if let Err(e) = std::fs::write(final_path, data) {
                    eprintln!("Error writing log channel {i}: {e}")
                }
            }
        }
    }

    fn select_tab(&mut self, n: usize) {
        if n < self.tabs.len() {
            self.current_tab = n;
        }
    }

    fn next_tab(&mut self) {
        self.select_tab((self.current_tab + 1) % self.tabs.len());
    }

    fn previous_tab(&mut self) {
        self.select_tab((self.current_tab + self.tabs.len() - 1) % self.tabs.len());
    }
}

fn layout_chunks(f: &mut ratatui::Frame, has_down_channel: bool) -> Rc<[Rect]> {
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
        .split(f.area())
}

fn render_tabs(f: &mut ratatui::Frame, chunk: Rect, tabs: &[Tab], current_tab: usize) {
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
