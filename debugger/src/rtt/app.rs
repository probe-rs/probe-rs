use super::channel::{ChannelState, DataFormat, Packet};
use anyhow::{anyhow, Result};
use probe_rs_rtt::RttChannel;
use std::collections::HashMap;
use std::io::{Read, Seek};

/// App holds the state of the application
pub struct App {
    tabs: Vec<ChannelState>,
}

fn pull_channel<C: RttChannel>(channels: &mut Vec<C>, n: usize) -> Option<C> {
    let c = channels
        .iter()
        .enumerate()
        .find_map(|(i, c)| if c.number() == n { Some(i) } else { None });

    c.map(|c| channels.remove(c))
}

impl App {
    pub fn new(mut rtt: probe_rs_rtt::Rtt, config: &crate::debugger::RttConfig) -> Result<Self> {
        let mut tabs = Vec::new();
        if !config.channels.is_empty() {
            let mut up_channels = rtt.up_channels().drain().collect::<Vec<_>>();
            let mut down_channels = rtt.down_channels().drain().collect::<Vec<_>>();
            for channel in &config.channels {
                tabs.push(ChannelState::new(
                    channel.up.and_then(|up| pull_channel(&mut up_channels, up)),
                    channel
                        .down
                        .and_then(|down| pull_channel(&mut down_channels, down)),
                    channel.name.clone(),
                    config.show_timestamps,
                    channel.format,
                ))
            }
        } else {
            let up_channels = rtt.up_channels().drain();
            let mut down_channels = rtt.down_channels().drain().collect::<Vec<_>>();
            for channel in up_channels.into_iter() {
                let number = channel.number();
                tabs.push(ChannelState::new(
                    Some(channel),
                    pull_channel(&mut down_channels, number),
                    None,
                    config.show_timestamps,
                    DataFormat::String,
                ));
            }

            for channel in down_channels {
                tabs.push(ChannelState::new(
                    None,
                    Some(channel),
                    None,
                    config.show_timestamps,
                    DataFormat::String,
                ));
            }
        }

        // Code farther down relies on tabs being configured and might panic
        // otherwise.
        if tabs.is_empty() {
            // TODO: Update to reflect move away RTT UI
            return Err(anyhow!(
                "Failed to initialize RTT UI: No RTT channels configured"
            ));
        }

        Ok(Self { tabs })
    }

    pub fn get_rtt_symbol<T: Read + Seek>(file: &mut T) -> Option<u64> {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_ok() {
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

    // pub fn render(
    //     &mut self,
    //     defmt_state: &Option<(defmt_decoder::Table, Option<defmt_decoder::Locations>)>,
    // ) {
    // binle_or_defmt => {
    //     self.terminal
    //         .draw(|f| {
    //             let constraints = if has_down_channel {
    //                 &[
    //                     Constraint::Length(1),
    //                     Constraint::Min(1),
    //                     Constraint::Length(1),
    //                 ][..]
    //             } else {
    //                 &[Constraint::Length(1), Constraint::Min(1)][..]
    //             };
    //             let chunks = Layout::default()
    //                 .direction(Direction::Vertical)
    //                 .margin(0)
    //                 .constraints(constraints)
    //                 .split(f.size());

    //             let tab_names = tabs
    //                 .iter()
    //                 .map(|t| Spans::from(t.name()))
    //                 .collect::<Vec<_>>();
    //             let tabs = Tabs::new(tab_names)
    //                 .select(current_tab)
    //                 .style(Style::default().fg(Color::Black).bg(Color::Yellow))
    //                 .highlight_style(
    //                     Style::default()
    //                         .fg(Color::Green)
    //                         .bg(Color::Yellow)
    //                         .add_modifier(Modifier::BOLD),
    //                 );
    //             f.render_widget(tabs, chunks[0]);

    //             height = chunks[1].height as usize;

    //             // probably pretty bad
    //             match binle_or_defmt {
    //                 DataFormat::BinaryLE => {
    //                     messages_wrapped.push(data.iter().fold(
    //                         String::new(),
    //                         |mut output, byte| {
    //                             let _ = write(&mut output, format_args!("{:#04x}, ", byte));
    //                             output
    //                         },
    //                     ));
    //                 }
    //                 DataFormat::Defmt => {
    //                     let (table, locs) = defmt_state.as_ref().expect(
    //                     "Running rtt in defmt mode but table or locations could not be loaded.",
    //                 );
    //                     let mut frames = vec![];

    //                     frames.extend_from_slice(&data);

    //                     while let Ok((frame, consumed)) =
    //                         table.decode(&frames)
    //                     {
    //                         // NOTE(`[]` indexing) all indices in `table` have already been
    //                         // verified to exist in the `locs` map.
    //                         let loc = locs.as_ref().map(|locs| &locs[&frame.index()]);

    //                         messages_wrapped.push(format!("{}", frame.display(false)));
    //                         if let Some(loc) = loc {
    //                             let relpath = if let Ok(relpath) =
    //                                 loc.file.strip_prefix(&std::env::current_dir().unwrap())
    //                             {
    //                                 relpath
    //                             } else {
    //                                 // not relative; use full path
    //                                 &loc.file
    //                             };

    //                             messages_wrapped.push(format!(
    //                                 "└─ {}:{}",
    //                                 relpath.display(),
    //                                 loc.line
    //                             ));
    //                         }

    //                         let num_frames = frames.len();
    //                         frames.rotate_left(consumed);
    //                         frames.truncate(num_frames - consumed);
    //                     }
    //                 }
    //                 DataFormat::String => unreachable!("You encountered a bug. Please open an issue on Github."),
    //             }

    //             let message_num = messages_wrapped.len();

    //             let messages: Vec<ListItem> = messages_wrapped
    //                 .iter()
    //                 .skip(message_num - (height + scroll_offset).min(message_num))
    //                 .take(height)
    //                 .map(|s| ListItem::new(vec![Spans::from(Span::raw(s))]))
    //                 .collect();

    //             let messages = List::new(messages.as_slice())
    //                 .block(Block::default().borders(Borders::NONE));
    //             f.render_widget(messages, chunks[1]);

    //             if has_down_channel {
    //                 let input = Paragraph::new(Spans::from(vec![Span::raw(input.clone())]))
    //                     .style(Style::default().fg(Color::Yellow).bg(Color::Blue));
    //                 f.render_widget(input, chunks[2]);
    //             }
    //         })
    //         .unwrap();

    //     let message_num = messages_wrapped.len();
    //     let scroll_offset = self.tabs[self.current_tab].scroll_offset();
    //     if message_num < height + scroll_offset {
    //         self.current_tab_mut()
    //             .set_scroll_offset(message_num - height.min(message_num));
    //     }
    // }
    //}

    /// Polls the RTT target for new data on all channels.
    pub fn poll_rtt(&mut self) -> HashMap<String, Packet> {
        self.tabs
            .iter_mut()
            .filter_map(|tab| {
                tab.poll_rtt()
                    .map(|packet| (tab.number().unwrap_or(0).to_string(), packet))
                // If the Channel doesn't have a number, then send the output to channel 0
            })
            .collect::<HashMap<_, _>>()
    }

    // pub fn push_rtt(&mut self) {
    //     self.tabs[self.current_tab].push_rtt();
    // }
}
