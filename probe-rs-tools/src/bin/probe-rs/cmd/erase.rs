use std::cell::RefCell;

use indicatif::{MultiProgress, ProgressBar};
use probe_rs::{
    flashing::{erase_all, FlashProgress, ProgressEvent},
    probe::list::Lister,
};

use crate::util::{common_options::ProbeOptions, flash::ProgressBarGroup, logging};

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let (mut session, _probe_options) = self.common.simple_attach(lister)?;

        let multi_progress = MultiProgress::new();
        logging::set_progress_bar(multi_progress.clone());

        let progress_bars = RefCell::new(ProgressBarGroup::new("Erasing"));

        let progress = FlashProgress::new(move |event| {
            let mut progress_bars = progress_bars.borrow_mut();

            match event {
                ProgressEvent::Initialized {
                    chip_erase, phases, ..
                } => {
                    // Build progress bars.
                    if chip_erase {
                        progress_bars.add(multi_progress.add(ProgressBar::new(0)));
                    }

                    if !chip_erase {
                        if phases.len() > 1 {
                            progress_bars.append_phase();
                        }

                        for phase_layout in phases {
                            let sector_size =
                                phase_layout.sectors().iter().map(|s| s.size()).sum::<u64>();
                            progress_bars.add(multi_progress.add(ProgressBar::new(sector_size)));
                        }
                    }
                }
                ProgressEvent::SectorErased { size, .. } => progress_bars.inc(size),
                ProgressEvent::FailedErasing => progress_bars.abandon(),
                ProgressEvent::FinishedErasing => progress_bars.finish(),
                _ => {}
            }
        });

        erase_all(&mut session, progress)?;

        Ok(())
    }
}
