use postcard_rpc::{TopicDirection, endpoints, topics};

use crate::breakpoints::{
    ResolveSourceBreakpointsRequest, ResolveSourceBreakpointsResponse,
    ResolveSourceLocationsRequest, ResolveSourceLocationsResponse,
};
use crate::chip::{ChipInfoRequest, ChipInfoResponse, ListFamiliesResponse, LoadChipFamilyRequest};
use crate::core_ops::{
    CoreAccessRequest, CoreBreakpointsRequest, CoreDumpRequest, CoreHaltRequest,
    CoreReadRegistersRequest, CoreVectorCatchRequest, CoreWriteRegRequest,
    HandleSemihostingRequest, HandleSemihostingResponse, StepRequest, StepResult, WireCoreDump,
    WireCoreInformation, WireCoreMetadata, WireCoreStatus, WireRegisterReadResult,
};
use crate::cores::{CoresRequest, CoresStatusResponse, HaltCoresRequest};
use crate::debug_vars::{
    ClearCoreDebugStateRequest, EvaluateRequest, EvaluateResponse, LoadSvdRequest, LoadSvdResponse,
    ScopesRequest, ScopesResponse, SetVariableRequest, SetVariableResult, VariablesRequest,
    VariablesResponse,
};
use crate::disassemble::{DisassembleRequest, DisassembleResponse};
use crate::file::{AppendFileRequest, CreateFileResponse};
use crate::flash::{
    BootRequest, BuildRequest, BuildResponse, EraseAllRequest, EraseRangeRequest, FlashRequest,
    LoadRegionRequest, NewFlashLoaderRequest, NewFlashLoaderResponse, ProgressEvent, VerifyRequest,
    VerifyResponse,
};
use crate::info::{InfoEvent, TargetInfoRequest, TargetMetadataRequest, TargetMetadataResponse};
use crate::memory::{ReadBytesRequest, ReadMemoryRequest, WriteMemoryRequest};
use crate::monitor::{MonitorRequest, MonitorResponse, RttEvent, SemihostingEvent};
use crate::probe::{
    AttachRequest, AttachResponse, ListProbesResponse, SelectProbeRequest, SelectProbeResponse,
};
use crate::reset::{ResetCoreAndHaltRequest, ResetCoreRequest};
use crate::rtt_client::{
    CreateRttClientRequest, CreateRttClientResponse, PollRttUpRequest, PollRttUpResponse,
    RttChannelRequest, RttChannelsResponse, RttDownRequest, RttDownResponse,
};
use crate::stack_trace::{
    LoadDebugInfoRequest, LoadDebugInfoResponse, TakeRichStackTraceRequest,
    TakeRichStackTraceResponse, TakeStackTraceRequest, TakeStackTraceResponse,
};
use crate::test::{
    ListTestsRequest, ListTestsResponse, RunTestRequest, RunTestResponse, TestKickoffRequest,
    TestKickoffResponse,
};
use crate::{NoResponse, RpcError, RpcResult};

type ReadMemory8Response = RpcResult<Vec<u8>>;
type ReadMemory16Response = RpcResult<Vec<u16>>;
type ReadMemory32Response = RpcResult<Vec<u32>>;
type ReadMemory64Response = RpcResult<Vec<u64>>;
type ReadBytesResponse = RpcResult<Vec<u8>>;

type SetVariableResponse = SetVariableResult;

type WriteMemory8Request = WriteMemoryRequest<u8>;
type WriteMemory16Request = WriteMemoryRequest<u16>;
type WriteMemory32Request = WriteMemoryRequest<u32>;
type WriteMemory64Request = WriteMemoryRequest<u64>;

type CoreStatusResponse = RpcResult<WireCoreStatus>;
type CoreInfoResponse = RpcResult<WireCoreInformation>;
type ResetAndHaltResponse = RpcResult<WireCoreInformation>;
type CoreMetadataResponse = RpcResult<WireCoreMetadata>;
type CoreReadRegistersResponse = RpcResult<Vec<WireRegisterReadResult>>;
type CoreDumpResponse = RpcResult<WireCoreDump>;
type CoreSetHwBpsResponse = RpcResult<Vec<Result<(), RpcError>>>;

endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy                | RequestTy               | ResponseTy              | Path               |
    | ----------                | ---------               | ----------              | ----               |
    | ListProbesEndpoint        | ()                      | ListProbesResponse      | "probe/list"       |
    | SelectProbeEndpoint       | SelectProbeRequest      | SelectProbeResponse     | "probe/select"     |
    | AttachEndpoint            | AttachRequest           | AttachResponse          | "probe/attach"     |

    | HaltCoresEndpoint         | HaltCoresRequest        | CoresStatusResponse     | "cores/halt"       |
    | ResumeCoresEndpoint       | CoresRequest            | CoresStatusResponse     | "cores/resume"     |
    | CoresStatusEndpoint       | CoresRequest            | CoresStatusResponse     | "cores/status"     |
    | NewFlashLoaderEndpoint    | NewFlashLoaderRequest   | NewFlashLoaderResponse  | "flash/new"        |
    | BuildEndpoint             | BuildRequest            | BuildResponse           | "flash/build"      |
    | LoadRegionEndpoint        | LoadRegionRequest       | NoResponse              | "flash/load_region"|
    | FlashEndpoint             | FlashRequest            | NoResponse              | "flash/flash"      |
    | EraseAllEndpoint          | EraseAllRequest         | NoResponse              | "flash/erase_all"  |
    | EraseRangeEndpoint        | EraseRangeRequest       | NoResponse              | "flash/erase_range"|
    | VerifyEndpoint            | VerifyRequest           | VerifyResponse          | "flash/verify"     |
    | BootEndpoint              | BootRequest             | NoResponse              | "flash/boot"       |
    | MonitorEndpoint           | MonitorRequest          | MonitorResponse         | "monitor"          |

    | TakeStackTraceEndpoint     | TakeStackTraceRequest     | TakeStackTraceResponse     | "stack_trace"              |
    | TakeRichStackTraceEndpoint | TakeRichStackTraceRequest | TakeRichStackTraceResponse | "stack_trace/rich"         |
    | ScopesEndpoint             | ScopesRequest             | ScopesResponse             | "stack_trace/scopes"       |
    | VariablesEndpoint          | VariablesRequest          | VariablesResponse          | "stack_trace/variables"    |
    | EvaluateEndpoint           | EvaluateRequest           | EvaluateResponse           | "stack_trace/evaluate"     |
    | SetVariableEndpoint        | SetVariableRequest        | SetVariableResponse        | "stack_trace/set_variable" |

    | LoadDebugInfoEndpoint            | LoadDebugInfoRequest            | LoadDebugInfoResponse            | "debug_state/load_debug_info"            |
    | ResolveSourceBreakpointsEndpoint | ResolveSourceBreakpointsRequest | ResolveSourceBreakpointsResponse | "debug_state/resolve_source_breakpoints" |
    | ResolveSourceLocationsEndpoint   | ResolveSourceLocationsRequest   | ResolveSourceLocationsResponse   | "debug_state/resolve_source_locations"   |
    | ClearCoreDebugStateEndpoint      | ClearCoreDebugStateRequest      | NoResponse                       | "debug_state/clear_core"                 |
    | LoadSvdEndpoint                  | LoadSvdRequest                  | LoadSvdResponse                  | "debug_state/load_svd"                   |

    | CreateRttClientEndpoint      | CreateRttClientRequest | CreateRttClientResponse | "create_rtt"              |
    | RttDownEndpoint              | RttDownRequest         | RttDownResponse         | "rtt/down"                |
    | GetRttChannelsEndpoint       | RttChannelRequest      | RttChannelsResponse     | "rtt/channels"            |
    | PollRttUpEndpoint            | PollRttUpRequest       | PollRttUpResponse       | "rtt/poll_up"             |
    | CleanUpRttEndpoint           | RttChannelRequest      | NoResponse              | "rtt/clean_up"            |
    | ClearRttControlBlockEndpoint | RttChannelRequest      | NoResponse              | "rtt/clear_control_block" |

    | ListTestsEndpoint         | ListTestsRequest        | ListTestsResponse       | "tests/list"       |
    | RunTestEndpoint           | RunTestRequest          | RunTestResponse         | "tests/run"        |
    | TestKickoffEndpoint       | TestKickoffRequest      | TestKickoffResponse     | "tests/kickoff"    |

    | CreateTempFileEndpoint    | ()                      | CreateFileResponse      | "temp_file/new"    |
    | TempFileDataEndpoint      | AppendFileRequest       | NoResponse              | "temp_file/append" |

    | ListChipFamiliesEndpoint  | ()                      | ListFamiliesResponse    | "chips/list"       |
    | ChipInfoEndpoint          | ChipInfoRequest         | ChipInfoResponse        | "chips/info"       |
    | LoadChipFamilyEndpoint    | LoadChipFamilyRequest   | NoResponse              | "chips/load"       |

    | TargetMetadataEndpoint    | TargetMetadataRequest   | TargetMetadataResponse  | "target/metadata"  |
    | TargetInfoEndpoint        | TargetInfoRequest       | NoResponse              | "info"             |
    | ResetCoreEndpoint         | ResetCoreRequest        | NoResponse              | "reset"            |
    | ResetCoreAndHaltEndpoint  | ResetCoreAndHaltRequest | ResetAndHaltResponse    | "reset_and_halt"   |

    | CoreStatusEndpoint           | CoreAccessRequest        | CoreStatusResponse         | "core/status"             |
    | CoreHaltEndpoint             | CoreHaltRequest          | CoreInfoResponse           | "core/halt"               |
    | CoreRunEndpoint              | CoreAccessRequest        | NoResponse                 | "core/run"                |
    | CoreStepEndpoint             | StepRequest              | StepResult                 | "core/step"               |
    | CoreWriteRegEndpoint         | CoreWriteRegRequest      | NoResponse                 | "core/write_reg"          |
    | CoreSetHwBpsEndpoint         | CoreBreakpointsRequest   | CoreSetHwBpsResponse       | "core/set_hw_bps"         |
    | CoreClearHwBpsEndpoint       | CoreBreakpointsRequest   | NoResponse                 | "core/clear_hw_bps"       |
    | CoreEnableVcEndpoint         | CoreVectorCatchRequest   | NoResponse                 | "core/enable_vc"          |
    | CoreMetadataEndpoint         | CoreAccessRequest        | CoreMetadataResponse       | "core/metadata"           |
    | CoreReadRegistersEndpoint    | CoreReadRegistersRequest | CoreReadRegistersResponse  | "core/read_registers"     |
    | CoreDumpEndpoint             | CoreDumpRequest          | CoreDumpResponse           | "core/dump"               |
    | HandleSemihostingEndpoint    | HandleSemihostingRequest | HandleSemihostingResponse  | "core/handle_semihosting" |
    | DisassembleEndpoint          | DisassembleRequest       | DisassembleResponse        | "core/disassemble"        |

    | ReadMemory8Endpoint       | ReadMemoryRequest       | ReadMemory8Response     | "memory/read8"      |
    | ReadMemory16Endpoint      | ReadMemoryRequest       | ReadMemory16Response    | "memory/read16"     |
    | ReadMemory32Endpoint      | ReadMemoryRequest       | ReadMemory32Response    | "memory/read32"     |
    | ReadMemory64Endpoint      | ReadMemoryRequest       | ReadMemory64Response    | "memory/read64"     |
    | ReadBytesEndpoint         | ReadBytesRequest        | ReadBytesResponse       | "memory/read_bytes" |

    | WriteMemory8Endpoint      | WriteMemory8Request     | NoResponse              | "memory/write8"    |
    | WriteMemory16Endpoint     | WriteMemory16Request    | NoResponse              | "memory/write16"   |
    | WriteMemory32Endpoint     | WriteMemory32Request    | NoResponse              | "memory/write32"   |
    | WriteMemory64Endpoint     | WriteMemory64Request    | NoResponse              | "memory/write64"   |
}

topics! {
    list = TOPICS_IN_LIST;
    direction = TopicDirection::ToServer;
    | TopicTy     | MessageTy     | Path     |
    | -------     | ---------     | ----     |
    | CancelTopic | ()            | "cancel" |
}

topics! {
    list = TOPICS_OUT_LIST;
    direction = TopicDirection::ToClient;
    | TopicTy             | MessageTy        | Path             | Cfg |
    | -------             | ---------        | ----             | --- |
    | TargetInfoDataTopic | InfoEvent        | "info/data"      |     |
    | ProgressEventTopic  | ProgressEvent    | "flash/progress" |     |
    | RttTopic            | RttEvent         | "rtt"            |     |
    | SemihostingTopic    | SemihostingEvent | "semihosting"    |     |
}
