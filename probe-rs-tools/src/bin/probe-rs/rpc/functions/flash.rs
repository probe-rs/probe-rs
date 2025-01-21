use std::{cell::Cell, rc::Rc};

use postcard_rpc::{
    header::{VarHeader, VarSeq},
    server::SpawnContext,
};
use postcard_schema::Schema;
use probe_rs::{
    flashing::{FileDownloadError, FlashProgress},
    rtt::ScanRegion,
    Session,
};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::{
        functions::{ProgressEventTopic, RpcContext, RpcResult, RpcSpawnContext},
        Key,
    },
    util::{flash::build_loader, rtt::client::RttClient},
    FormatOptions,
};

#[derive(Serialize, Deserialize, Default, Schema)]
pub struct DownloadOptions {
    /// If `keep_unwritten_bytes` is `true`, erased portions of the flash that are not overwritten by the ELF data
    /// are restored afterwards, such that the old contents are untouched.
    ///
    /// This is necessary because the flash can only be erased in sectors. If only parts of the erased sector are written thereafter,
    /// instead of the full sector, the excessively erased bytes wont match the contents before the erase which might not be intuitive
    /// to the user or even worse, result in unexpected behavior if those contents contain important data.
    pub keep_unwritten_bytes: bool,
    /// If this flag is set to true, probe-rs will try to use the chips built in method to do a full chip erase if one is available.
    /// This is often faster than erasing a lot of single sectors.
    /// So if you do not need the old contents of the flash, this is a good option.
    pub do_chip_erase: bool,
    /// If the chip was pre-erased with external erasers, this flag can set to true to skip erasing
    /// It may be useful for mass production.
    pub skip_erase: bool,
    /// Before flashing, read back the flash contents to skip up-to-date regions.
    pub preverify: bool,
    /// After flashing, read back all the flashed data to verify it has been written correctly.
    pub verify: bool,
    /// Disable double buffering when loading flash.
    pub disable_double_buffering: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct FlashRequest {
    pub sessid: Key<Session>,
    pub path: String,
    pub format: FormatOptions,
    pub options: DownloadOptions,
    pub rtt_client: Option<Key<RttClient>>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct FlashResult {
    pub boot_info: BootInfo,
    pub flash_layout: Vec<FlashLayout>,
}

pub type FlashResponse = RpcResult<FlashResult>;

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashLayout {
    pub sectors: Vec<FlashSector>,
    pub pages: Vec<FlashPage>,
    pub fills: Vec<FlashFill>,
    pub data_blocks: Vec<FlashDataBlockSpan>,
}

impl From<probe_rs::flashing::FlashLayout> for FlashLayout {
    fn from(layout: probe_rs::flashing::FlashLayout) -> Self {
        FlashLayout {
            sectors: layout
                .sectors()
                .iter()
                .map(|sector| FlashSector {
                    address: sector.address(),
                    size: sector.size(),
                })
                .collect(),
            pages: layout
                .pages()
                .iter()
                .map(|page| FlashPage {
                    address: page.address(),
                    data_len: page.data().len() as u64,
                })
                .collect(),
            fills: layout
                .fills()
                .iter()
                .map(|fill| FlashFill {
                    address: fill.address(),
                    size: fill.size(),
                    page_index: fill.page_index() as u64,
                })
                .collect(),
            data_blocks: layout
                .data_blocks()
                .iter()
                .map(|block| FlashDataBlockSpan {
                    address: block.address(),
                    size: block.size(),
                })
                .collect(),
        }
    }
}

/// The description of a page in flash.
#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashPage {
    pub address: u64,
    pub data_len: u64,
}

/// The description of a sector in flash.
#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashSector {
    pub address: u64,
    pub size: u64,
}

/// A struct to hold all the information about one region
/// in the flash that is erased during flashing and has to be restored to its original value afterwards.
#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashFill {
    pub address: u64,
    pub size: u64,
    pub page_index: u64,
}

/// A block of data that is to be written to flash.
#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashDataBlockSpan {
    address: u64,
    size: u64,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub enum ProgressEvent {
    /// The flash layout has been built and the flashing procedure was initialized.
    Initialized {
        /// Whether the chip erase feature is enabled.
        /// If this is true, the chip will be erased before any other operation. No separate erase
        /// progress bars are necessary in this case.
        chip_erase: bool,

        /// The layout of the flash contents as it will be used by the flash procedure, grouped by
        /// phases (fill, erase, program sequences).
        /// This is an exact report of what the flashing procedure will do during the flashing process.
        phases: Vec<FlashLayout>,

        /// Whether the unwritten flash contents will be restored after erasing.
        restore_unwritten: bool,
    },
    /// Filling of flash pages has started.
    StartedFilling,
    /// A page has been filled successfully.
    /// This does not mean the page has been programmed yet.
    /// Only its contents are determined at this point!
    PageFilled {
        /// The size of the page in bytes.
        size: u64,
    },
    /// Filling of the pages has failed.
    FailedFilling,
    /// Filling of the pages has finished successfully.
    FinishedFilling,
    /// Erasing of flash has started.
    StartedErasing,
    /// A sector has been erased successfully.
    SectorErased {
        /// The size of the sector in bytes.
        size: u64,
    },
    /// Erasing of the flash has failed.
    FailedErasing,
    /// Erasing of the flash has finished successfully.
    FinishedErasing,
    /// Programming of the flash has started.
    StartedProgramming {
        /// The total length of the data to be programmed in bytes.
        length: u64,
    },
    /// A flash page has been programmed successfully.
    PageProgrammed {
        /// The size of this page in bytes.
        size: u32,
    },
    /// Programming of the flash failed.
    FailedProgramming,
    /// Programming of the flash has finished successfully.
    FinishedProgramming,
    /// a message was received from the algo.
    DiagnosticMessage {
        /// The message that was emitted.
        message: String,
    },
}

impl From<probe_rs::flashing::ProgressEvent> for ProgressEvent {
    fn from(event: probe_rs::flashing::ProgressEvent) -> Self {
        match event {
            probe_rs::flashing::ProgressEvent::Initialized {
                chip_erase,
                phases,
                restore_unwritten,
            } => ProgressEvent::Initialized {
                chip_erase,
                phases: phases.into_iter().map(Into::into).collect(),
                restore_unwritten,
            },
            probe_rs::flashing::ProgressEvent::StartedFilling => ProgressEvent::StartedFilling,
            probe_rs::flashing::ProgressEvent::PageFilled { size, .. } => {
                ProgressEvent::PageFilled { size }
            }
            probe_rs::flashing::ProgressEvent::FailedFilling => ProgressEvent::FailedFilling,
            probe_rs::flashing::ProgressEvent::FinishedFilling => ProgressEvent::FinishedFilling,
            probe_rs::flashing::ProgressEvent::StartedErasing => ProgressEvent::StartedErasing,
            probe_rs::flashing::ProgressEvent::SectorErased { size, .. } => {
                ProgressEvent::SectorErased { size }
            }
            probe_rs::flashing::ProgressEvent::FailedErasing => ProgressEvent::FailedErasing,
            probe_rs::flashing::ProgressEvent::FinishedErasing => ProgressEvent::FinishedErasing,
            probe_rs::flashing::ProgressEvent::StartedProgramming { length } => {
                ProgressEvent::StartedProgramming { length }
            }
            probe_rs::flashing::ProgressEvent::PageProgrammed { size, .. } => {
                ProgressEvent::PageProgrammed { size }
            }
            probe_rs::flashing::ProgressEvent::FailedProgramming => {
                ProgressEvent::FailedProgramming
            }
            probe_rs::flashing::ProgressEvent::FinishedProgramming => {
                ProgressEvent::FinishedProgramming
            }
            probe_rs::flashing::ProgressEvent::DiagnosticMessage { message } => {
                ProgressEvent::DiagnosticMessage { message }
            }
        }
    }
}

/// Current boot information
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub enum BootInfo {
    /// Loaded executable has a vector table in RAM
    FromRam {
        /// Address of the vector table in memory
        vector_table_addr: u64,
        /// All cores that should be reset and halted before any RAM access
        cores_to_reset: Vec<String>,
    },
    /// Executable is either not loaded yet or will be booted conventionally (from flash etc.)
    Other,
}

impl From<probe_rs::flashing::BootInfo> for BootInfo {
    fn from(boot_info: probe_rs::flashing::BootInfo) -> Self {
        match boot_info {
            probe_rs::flashing::BootInfo::FromRam {
                vector_table_addr,
                cores_to_reset,
            } => BootInfo::FromRam {
                vector_table_addr,
                cores_to_reset,
            },
            probe_rs::flashing::BootInfo::Other => BootInfo::Other,
        }
    }
}

pub async fn flash(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: FlashRequest,
) -> FlashResponse {
    let ctx = ctx.spawn_ctxt();
    tokio::task::spawn_blocking(move || flash_impl(ctx, request))
        .await
        .unwrap()
}

fn flash_impl(ctx: RpcSpawnContext, request: FlashRequest) -> FlashResponse {
    let dry_run = ctx.dry_run(request.sessid);
    let mut session = ctx.session_blocking(request.sessid);

    let mut rtt_client = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    // build loader
    let loader = build_loader(&mut session, &request.path, request.format, None)?;

    // When using RTT with a program in flash, the RTT header will be moved to RAM on
    // startup, so clearing it before startup is ok. However, if we're downloading to the
    // header's final address in RAM, then it's not relocated on startup and we should not
    // clear it. This impacts static RTT headers, like used in defmt_rtt.
    let should_clear_rtt_header = if let Some(rtt_client) = rtt_client.as_ref() {
        if let ScanRegion::Exact(address) = rtt_client.scan_region {
            tracing::debug!("RTT ScanRegion::Exact address is within region to be flashed");
            !loader.has_data_for_address(address)
        } else {
            true
        }
    } else {
        false
    };

    let flash_layout = Rc::new(Cell::new(vec![]));

    let mut options = probe_rs::flashing::DownloadOptions::default();

    options.keep_unwritten_bytes = request.options.keep_unwritten_bytes;
    options.dry_run = dry_run;
    options.do_chip_erase = request.options.do_chip_erase;
    options.skip_erase = request.options.skip_erase;
    options.preverify = request.options.preverify;
    options.verify = request.options.verify;
    options.disable_double_buffering = request.options.disable_double_buffering;
    options.progress = Some(FlashProgress::new({
        let flash_layout = flash_layout.clone();
        let ctx = ctx.clone();
        move |event| {
            let event = ProgressEvent::from(event);
            if let ProgressEvent::Initialized { ref phases, .. } = event {
                flash_layout.set(phases.clone());
            }

            ctx.publish_blocking::<ProgressEventTopic>(VarSeq::Seq2(0), event)
                .unwrap();
        }
    }));

    // run flash download
    loader
        .commit(&mut session, options)
        .map_err(FileDownloadError::Flash)?;

    let boot_info = loader.boot_info();

    if let Some(rtt_client) = rtt_client.as_mut() {
        if should_clear_rtt_header {
            // We ended up resetting the MCU, throw away old RTT data and prevent
            // printing warnings when it initialises.
            let mut core = session.core(rtt_client.core_id())?;
            rtt_client.clear_control_block(&mut core)?;
            tracing::debug!("Cleared RTT header");
        }
    }

    Ok(FlashResult {
        boot_info: boot_info.into(),
        flash_layout: flash_layout.take(),
    })
}
