use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::format::FormatOptions;
use crate::{FlashLoader, Key, RpcResult, RttClient, Session};

#[derive(Serialize, Deserialize, Default, Schema)]
pub struct DownloadOptions {
    pub keep_unwritten_bytes: bool,
    pub do_chip_erase: bool,
    pub skip_erase: bool,
    pub verify: bool,
    pub disable_double_buffering: bool,
    pub preferred_algos: Vec<String>,
    pub ram_chunk_size: Option<u64>,
}

impl DownloadOptions {
    pub fn sanitize(&mut self) {
        if !self.preferred_algos.is_empty() {
            for algo in self.preferred_algos.iter_mut() {
                *algo = algo
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
            }
            self.preferred_algos.retain(|s| !s.is_empty());
        }
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct NewFlashLoaderRequest {
    pub sessid: Key<Session>,
    pub read_flasher_rtt: bool,
}

pub type NewFlashLoaderResponse = RpcResult<Key<FlashLoader>>;

#[derive(Serialize, Deserialize, Schema)]
pub struct LoadRegionRequest {
    pub sessid: Key<Session>,
    pub loader: Key<FlashLoader>,
    pub address: u64,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct BuildRequest {
    pub sessid: Key<Session>,
    pub path: String,
    pub format: FormatOptions,
    pub image_target: Option<String>,
    pub read_flasher_rtt: bool,
    /// RTT client to configure from the image. The image tells the client
    /// whether the download writes the RTT control block, which the client
    /// must then not clear.
    pub rtt_client: Option<Key<RttClient>>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct BuildResult {
    pub loader: Key<FlashLoader>,
    pub boot_info: BootInfo,
}

pub type BuildResponse = RpcResult<BuildResult>;

#[derive(Serialize, Deserialize, Schema)]
pub struct FlashRequest {
    pub sessid: Key<Session>,
    pub loader: Key<FlashLoader>,
    pub options: DownloadOptions,
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

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashPage {
    pub address: u64,
    pub data_len: u64,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashSector {
    pub address: u64,
    pub size: u64,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashFill {
    pub address: u64,
    pub size: u64,
    pub page_index: u64,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub struct FlashDataBlockSpan {
    pub address: u64,
    pub size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema, Hash)]
pub enum Operation {
    Fill,
    Erase,
    Program,
    Verify,
    Ram,
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub enum ProgressEvent {
    FlashLayoutReady {
        flash_layout: Vec<FlashLayout>,
    },
    AddProgressBar {
        operation: Operation,
        total: Option<u64>,
    },
    Started(Operation),
    Progress {
        operation: Operation,
        size: u64,
    },
    Failed(Operation),
    Finished(Operation),
    DiagnosticMessage {
        message: String,
    },
}

impl ProgressEvent {
    pub fn is_operation(&self, operation: Operation) -> bool {
        matches!(
            self,
            ProgressEvent::Started(op)
            | ProgressEvent::Progress { operation: op, .. }
            | ProgressEvent::Failed(op)
            | ProgressEvent::Finished(op)
            | ProgressEvent::AddProgressBar { operation: op, .. }
            if *op == operation
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub enum BootInfo {
    FromRam {
        vector_table_addr: u64,
        cores_to_reset: Vec<String>,
    },
    Other,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct BootRequest {
    pub sessid: Key<Session>,
    pub boot_info: BootInfo,
    pub core_id: u32,
    /// When true, resume all cores after prepare. When false, leave them halted.
    pub resume: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct EraseAllRequest {
    pub sessid: Key<Session>,
    pub read_flasher_rtt: bool,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct EraseRangeRequest {
    pub sessid: Key<Session>,
    pub address: u64,
    pub length: u64,
    /// When true, restore bytes that fall inside erased flash sectors but
    /// outside `[address, address + length)`. When false, those bordering
    /// bytes stay erased.
    pub restore: bool,
    pub read_flasher_rtt: bool,
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
