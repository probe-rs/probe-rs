pub struct FlashProgress {
    handler: Box<dyn Fn(ProgressEvent)>,
}

impl FlashProgress {
    pub fn new(handler: impl Fn(ProgressEvent) + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    pub fn emit(&self, event: ProgressEvent) {
        (self.handler)(event);
    }

    pub fn initialized(&self, total_sectors: usize, total_pages: usize) {
        self.emit(ProgressEvent::Initialized {
            total_sectors,
            total_pages,
        });
    }

    pub fn started_flashing(&self) {
        self.emit(ProgressEvent::StartedFlashing);
    }

    pub fn started_erasing(&self) {
        self.emit(ProgressEvent::StartedErasing);
    }

    pub fn page_programmed(&self, size: u32, time: u128) {
        self.emit(ProgressEvent::PageFlashed { size, time });
    }

    pub fn sector_erased(&self, size: u32, time: u128) {
        self.emit(ProgressEvent::SectorErased { size, time });
    }

    pub fn finished_programming(&self) {
        self.emit(ProgressEvent::FinishedProgramming);
    }

    pub fn finished_erasing(&self) {
        self.emit(ProgressEvent::FinishedErasing);
    }
}

pub enum ProgressEvent {
    Initialized {
        total_pages: usize,
        total_sectors: usize,
    },
    StartedFlashing,
    StartedErasing,
    PageFlashed {
        size: u32,
        time: u128,
    },
    SectorErased {
        size: u32,
        time: u128,
    },
    FinishedProgramming,
    FinishedErasing,
}
