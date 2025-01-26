use indicatif::MultiProgress;
use probe_rs::{
    flashing::{erase_all, FlashProgress},
    probe::list::Lister,
};

use crate::{
    rpc::functions::flash::{Operation, ProgressEvent},
    util::{common_options::ProbeOptions, flash::CliProgressBars, logging},
};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    #[arg(long, help_heading = "DOWNLOAD CONFIGURATION")]
    pub disable_progressbars: bool,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister)?;

        let multi_progress = MultiProgress::new();
        logging::set_progress_bar(multi_progress.clone());

        let progress = if !self.disable_progressbars {
            let progress_bars = CliProgressBars::new();

            FlashProgress::new(move |event| {
                ProgressEvent::from_library_event(event, |event| {
                    // Only handle Erase-related events.
                    if let ProgressEvent::AddProgressBar {
                        operation: Operation::Erase,
                        ..
                    }
                    | ProgressEvent::Started {
                        operation: Operation::Erase,
                        ..
                    }
                    | ProgressEvent::Progress {
                        operation: Operation::Erase,
                        ..
                    }
                    | ProgressEvent::Failed(Operation::Erase)
                    | ProgressEvent::Finished(Operation::Erase) = event
                    {
                        progress_bars.handle(event)
                    }
                });
            })
        } else {
            FlashProgress::empty()
        };

        erase_all(&mut session, progress)?;

        Ok(())
    }
}
