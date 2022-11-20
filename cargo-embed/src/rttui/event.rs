use std::sync::mpsc;
use std::sync::{atomic::AtomicBool, Arc};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event as CEvent, KeyEvent};

/// A small event handler that wrap termion input and tick events. Each event
/// type is handled in its own thread and returned to a common `Receiver`
pub struct Events {
    rx: mpsc::Receiver<KeyEvent>,
    _input_handle: thread::JoinHandle<()>,
    _ignore_exit_key: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub poll_rate: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            poll_rate: Duration::from_millis(10),
        }
    }
}

impl Events {
    pub fn new() -> Events {
        Self::with_config(Config::default())
    }

    pub fn with_config(config: Config) -> Events {
        let (tx, rx) = mpsc::channel();
        let ignore_exit_key = Arc::new(AtomicBool::new(false));
        let input_handle = {
            thread::Builder::new()
                .name("probe-rs-terminal-event-handler".to_owned())
                .spawn(move || {
                    loop {
                        // poll for tick rate duration, if no events, sent tick event.
                        if event::poll(config.poll_rate).unwrap() {
                            if let CEvent::Key(key) = event::read().unwrap() {
                                if tx.send(key).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                })
                .unwrap()
        };

        Events {
            rx,
            _ignore_exit_key: ignore_exit_key,
            _input_handle: input_handle,
        }
    }

    pub fn next(&self, timeout: Duration) -> Result<KeyEvent, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}
