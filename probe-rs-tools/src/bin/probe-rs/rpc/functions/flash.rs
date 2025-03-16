use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{
    Session,
    flashing::{self, FileDownloadError, FlashLoader, FlashProgress},
    rtt::ScanRegion,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;

use crate::{
    FormatOptions,
    rpc::{
        Key,
        functions::{NoResponse, ProgressEventTopic, RpcContext, RpcResult, RpcSpawnContext},
    },
    util::{flash::build_loader, rtt::client::RttClient},
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
    /// After flashing, read back all the flashed data to verify it has been written correctly.
    pub verify: bool,
    /// Disable double buffering when loading flash.
    pub disable_double_buffering: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct BuildRequest {
    pub sessid: Key<Session>,
    pub path: String,
    pub format: FormatOptions,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct BuildResult {
    pub loader: Key<FlashLoader>,
    pub boot_info: BootInfo,
}

pub type BuildResponse = RpcResult<BuildResult>;

pub async fn build(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: BuildRequest,
) -> BuildResponse {
    // build loader
    let mut session = ctx.session(request.sessid).await;
    let loader = build_loader(&mut session, &request.path, request.format, None)?;

    Ok(BuildResult {
        boot_info: loader.boot_info().into(),
        loader: ctx.store_object(loader).await,
    })
}

#[derive(Serialize, Deserialize, Schema)]
pub struct FlashRequest {
    pub sessid: Key<Session>,
    pub loader: Key<FlashLoader>,
    pub options: DownloadOptions,
    pub rtt_client: Option<Key<RttClient>>,
}

#[derive(Default, Clone, Serialize, Deserialize, Schema)]
pub struct FlashLayout {
    pub sectors: Vec<FlashSector>,
    pub pages: Vec<FlashPage>,
    pub fills: Vec<FlashFill>,
    pub data_blocks: Vec<FlashDataBlockSpan>,
}
impl FlashLayout {
    pub fn merge_from(&mut self, layout: FlashLayout) {
        self.sectors.extend(layout.sectors);
        self.pages.extend(layout.pages);
        self.fills.extend(layout.fills);
        self.data_blocks.extend(layout.data_blocks);
    }
}

impl From<&probe_rs::flashing::FlashLayout> for FlashLayout {
    fn from(layout: &probe_rs::flashing::FlashLayout) -> Self {
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
    pub address: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Serialize, Deserialize, Schema)]
pub enum Operation {
    /// Reading back flash contents to restore erased regions that should be kept unchanged.
    Fill,

    /// Erasing flash sectors.
    Erase,

    /// Writing data to flash.
    Program,

    /// Checking flash contents.
    Verify,
}

impl From<flashing::ProgressOperation> for Operation {
    fn from(operation: flashing::ProgressOperation) -> Self {
        match operation {
            flashing::ProgressOperation::Fill => Operation::Fill,
            flashing::ProgressOperation::Erase => Operation::Erase,
            flashing::ProgressOperation::Program => Operation::Program,
            flashing::ProgressOperation::Verify => Operation::Verify,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub enum ProgressEvent {
    FlashLayoutReady {
        flash_layout: Vec<FlashLayout>,
    },

    /// Display a new progress bar to the user.
    AddProgressBar {
        operation: Operation,
        total: Option<u64>, // None means indeterminate
    },

    /// Started an operation with the given total size.
    Started(Operation),

    /// An operation has made progress.
    Progress {
        operation: Operation,
        /// The size of the page in bytes.
        size: u64,
    },

    /// An operation has failed.
    Failed(Operation),

    /// An operation has finished successfully.
    Finished(Operation),

    /// A message was received from the algo.
    DiagnosticMessage {
        /// The message that was emitted.
        message: String,
    },
}
impl ProgressEvent {
    pub fn from_library_event(event: flashing::ProgressEvent, mut cb: impl FnMut(ProgressEvent)) {
        let event = match event {
            flashing::ProgressEvent::FlashLayoutReady { flash_layout } => {
                ProgressEvent::FlashLayoutReady {
                    flash_layout: flash_layout.iter().map(Into::into).collect(),
                }
            }
            flashing::ProgressEvent::AddProgressBar { operation, total } => {
                ProgressEvent::AddProgressBar {
                    operation: operation.into(),
                    total,
                }
            }
            flashing::ProgressEvent::Started(operation) => ProgressEvent::Started(operation.into()),
            flashing::ProgressEvent::Progress {
                operation, size, ..
            } => ProgressEvent::Progress {
                operation: operation.into(),
                size,
            },
            flashing::ProgressEvent::Failed(operation) => ProgressEvent::Failed(operation.into()),
            flashing::ProgressEvent::Finished(operation) => {
                ProgressEvent::Finished(operation.into())
            }
            flashing::ProgressEvent::DiagnosticMessage { message } => {
                ProgressEvent::DiagnosticMessage { message }
            }
        };

        cb(event);
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

pub async fn flash(ctx: &mut RpcContext, _header: VarHeader, request: FlashRequest) -> NoResponse {
    ctx.run_blocking::<ProgressEventTopic, _, _, _>(request, flash_impl)
        .await
}

fn flash_impl(
    ctx: RpcSpawnContext,
    request: FlashRequest,
    sender: Sender<ProgressEvent>,
) -> NoResponse {
    let dry_run = ctx.dry_run(request.sessid);
    let mut session = ctx.session_blocking(request.sessid);

    let mut rtt_client = request
        .rtt_client
        .map(|rtt_client| ctx.object_mut_blocking(rtt_client));

    // build loader
    let loader = ctx.object_mut_blocking(request.loader);

    if let Some(rtt_client) = rtt_client.as_mut() {
        // When using RTT with a program in flash, the RTT header will be moved to RAM on
        // startup, so clearing it before startup is ok. However, if we're downloading to the
        // header's final address in RAM, then it's not relocated on startup and we should not
        // clear it. This impacts static RTT headers, like used in defmt_rtt.

        if let ScanRegion::Exact(address) = rtt_client.scan_region {
            if loader.has_data_for_address(address) {
                tracing::debug!(
                    "RTT control block is initialized by flash loader. Disabling clearing."
                );
                rtt_client.disallow_clearing_rtt_header()
            }
        }
    }

    let mut options = probe_rs::flashing::DownloadOptions::default();

    options.keep_unwritten_bytes = request.options.keep_unwritten_bytes;
    options.dry_run = dry_run;
    options.do_chip_erase = request.options.do_chip_erase;
    options.skip_erase = request.options.skip_erase;
    options.preverify = false;
    options.verify = request.options.verify;
    options.disable_double_buffering = request.options.disable_double_buffering;
    options.progress = Some(FlashProgress::new(move |event| {
        ProgressEvent::from_library_event(event, |event| sender.blocking_send(event).unwrap());
    }));

    // run flash download
    loader
        .commit(&mut session, options)
        .map_err(FileDownloadError::Flash)?;

    Ok(())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct EraseRequest {
    pub sessid: Key<Session>,
    pub command: EraseCommand,
}

#[derive(Serialize, Deserialize, Schema)]
pub enum EraseCommand {
    All,
}

pub async fn erase(ctx: &mut RpcContext, _header: VarHeader, request: EraseRequest) -> NoResponse {
    ctx.run_blocking::<ProgressEventTopic, _, _, _>(request, erase_impl)
        .await
}

fn erase_impl(
    ctx: RpcSpawnContext,
    request: EraseRequest,
    sender: Sender<ProgressEvent>,
) -> NoResponse {
    let mut session = ctx.session_blocking(request.sessid);

    let progress = FlashProgress::new(move |event| {
        ProgressEvent::from_library_event(event, |event| {
            // Only emit Erase-related events.
            if let ProgressEvent::AddProgressBar {
                operation: Operation::Erase,
                ..
            }
            | ProgressEvent::Started(Operation::Erase)
            | ProgressEvent::Progress {
                operation: Operation::Erase,
                ..
            }
            | ProgressEvent::Failed(Operation::Erase)
            | ProgressEvent::Finished(Operation::Erase) = event
            {
                sender.blocking_send(event).unwrap()
            }
        });
    });

    match request.command {
        EraseCommand::All => flashing::erase_all(&mut session, progress)?,
    }

    Ok(())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct VerifyRequest {
    pub sessid: Key<Session>,
    pub loader: Key<FlashLoader>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Schema)]
pub enum VerifyResult {
    Ok,
    Mismatch,
}

pub type VerifyResponse = RpcResult<VerifyResult>;

pub async fn verify(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: VerifyRequest,
) -> VerifyResponse {
    ctx.run_blocking::<ProgressEventTopic, _, _, _>(request, verify_impl)
        .await
}

fn verify_impl(
    ctx: RpcSpawnContext,
    request: VerifyRequest,
    sender: Sender<ProgressEvent>,
) -> VerifyResponse {
    let mut session = ctx.session_blocking(request.sessid);
    let loader = ctx.object_mut_blocking(request.loader);

    let progress = FlashProgress::new(move |event| {
        ProgressEvent::from_library_event(event, |event| {
            // Only emit Erase-related events.
            if let ProgressEvent::AddProgressBar {
                operation: Operation::Verify,
                ..
            }
            | ProgressEvent::Started(Operation::Verify)
            | ProgressEvent::Progress {
                operation: Operation::Verify,
                ..
            }
            | ProgressEvent::Failed(Operation::Verify)
            | ProgressEvent::Finished(Operation::Verify) = event
            {
                sender.blocking_send(event).unwrap()
            }
        });
    });

    match loader.verify(&mut session, progress) {
        Ok(()) => Ok(VerifyResult::Ok),
        Err(flashing::FlashError::Verify) => Ok(VerifyResult::Mismatch),
        Err(other) => Err(other.into()),
    }
}
