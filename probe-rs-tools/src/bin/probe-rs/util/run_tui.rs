//! An on-demand TUI for any `run`-like command.
//!
//! Enters an alternate screen and renders the tail of each RTT channel's
//! captured output. Channels are shown as tabs at the top.

use std::io::{Stderr, stderr};

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

/// An on-demand TUI for any `run`-like command.
pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stderr>>,
}

/// One RTT channel worth of data to render.
pub struct Channel<'a> {
    pub name: &'a str,
    pub lines: &'a [String],
}

impl Tui {
    /// Enter the TUI and set up the shell accordingly.
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(stderr(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stderr()))?;
        Ok(Self { terminal })
    }

    /// Leave the TUI and restore the shell.
    pub fn exit(&mut self) -> Result<()> {
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        disable_raw_mode()?;
        Ok(())
    }

    /// Render the given channels with `selected` highlighted.
    pub fn draw(&mut self, channels: &[Channel<'_>], selected: usize) -> Result<()> {
        self.terminal
            .draw(|frame| render(frame, channels, selected))?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

/// Render a single frame of the TUI.
fn render(frame: &mut ratatui::Frame<'_>, channels: &[Channel<'_>], selected: usize) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let titles: Vec<Line<'_>> = channels.iter().map(|c| Line::from(c.name)).collect();
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" RTT "))
        .select(selected.min(channels.len().saturating_sub(1)));
    frame.render_widget(tabs, chunks[0]);

    let body_area = chunks[1];
    let body_block = Block::default().borders(Borders::ALL);
    let inner = body_block.inner(body_area);
    frame.render_widget(body_block, body_area);

    if let Some(channel) = channels.get(selected) {
        let visible = inner.height as usize;
        let start = channel.lines.len().saturating_sub(visible);
        let text: Text<'_> = channel.lines[start..]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect();
        frame.render_widget(Paragraph::new(text), inner);
    }
}
