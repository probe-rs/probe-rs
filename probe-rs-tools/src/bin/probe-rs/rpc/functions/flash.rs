use std::time::Duration;

use postcard_rpc::header::VarHeader;
use probe_rs::{
    InstructionSet,
    flashing::{self, FileDownloadError, FlashProgress},
};
use probe_rs_rpc::flash::{
    BootInfo, BootRequest, BuildRequest, BuildResponse, BuildResult, EraseAllRequest,
    EraseRangeRequest, FlashRequest, LoadRegionRequest, NewFlashLoaderRequest,
    NewFlashLoaderResponse, Operation, ProgressEvent, VerifyRequest, VerifyResponse, VerifyResult,
};
use tokio::sync::mpsc::Sender;

use probe_rs_rpc::{NoResponse, ProgressEventTopic};

use crate::{
    rpc::functions::{RpcContext, RpcSpawnContext, convert::lift},
    util::flash::build_loader,
};

fn flash_request_download_options(request: &FlashRequest) -> flashing::DownloadOptions<'_> {
    let mut options = probe_rs::flashing::DownloadOptions::default();

    options.keep_unwritten_bytes = request.options.keep_unwritten_bytes;
    options.do_chip_erase = request.options.do_chip_erase;
    options.skip_erase = request.options.skip_erase;
    options.preverify = false;
    options.verify = request.options.verify;
    options.disable_double_buffering = request.options.disable_double_buffering;
    options.preferred_algos = request.options.preferred_algos.clone();

    options
}

pub fn prepare_boot_info(
    boot_info: &BootInfo,
    session: &mut probe_rs::Session,
    core_id: usize,
) -> anyhow::Result<()> {
    match convert::from_wire_boot_info(boot_info) {
        flashing::BootInfo::FromRam {
            vector_table_addr, ..
        } => {
            session.prepare_running_on_ram(vector_table_addr, core_id)?;
        }
        flashing::BootInfo::Other => {
            session
                .core(core_id)?
                .reset_and_halt(Duration::from_millis(500))?;
        }
    }

    Ok(())
}

pub fn from_library_progress_event(
    event: flashing::ProgressEvent,
    mut cb: impl FnMut(ProgressEvent),
) {
    let event = match event {
        flashing::ProgressEvent::FlashLayoutReady { flash_layout } => {
            ProgressEvent::FlashLayoutReady {
                flash_layout: flash_layout
                    .iter()
                    .map(convert::to_wire_flash_layout)
                    .collect(),
            }
        }
        flashing::ProgressEvent::AddProgressBar { operation, total } => {
            ProgressEvent::AddProgressBar {
                operation: convert::to_wire_operation(operation),
                total,
            }
        }
        flashing::ProgressEvent::Started(operation) => {
            ProgressEvent::Started(convert::to_wire_operation(operation))
        }
        flashing::ProgressEvent::Progress {
            operation, size, ..
        } => ProgressEvent::Progress {
            operation: convert::to_wire_operation(operation),
            size,
        },
        flashing::ProgressEvent::Failed(operation) => {
            ProgressEvent::Failed(convert::to_wire_operation(operation))
        }
        flashing::ProgressEvent::Finished(operation) => {
            ProgressEvent::Finished(convert::to_wire_operation(operation))
        }
        flashing::ProgressEvent::DiagnosticMessage { message } => {
            ProgressEvent::DiagnosticMessage { message }
        }
    };

    cb(event);
}

pub async fn new_flash_loader(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: NewFlashLoaderRequest,
) -> NewFlashLoaderResponse {
    let session = ctx.session(request.sessid).await;
    let mut loader = session.target().flash_loader();
    loader.read_rtt_output(request.read_flasher_rtt);
    Ok(ctx.store_object(loader).await)
}

pub async fn load_region(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: LoadRegionRequest,
) -> NoResponse {
    let mut loader = ctx.object_mut(request.loader).await;
    lift(loader.add_data(request.address, &request.data))?;
    Ok(())
}

pub async fn build(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: BuildRequest,
) -> BuildResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut loader = lift(build_loader(
        &mut session,
        &request.path,
        request.format,
        request
            .image_target
            .as_deref()
            .and_then(InstructionSet::from_target_triple),
    ))?;

    loader.read_rtt_output(request.read_flasher_rtt);

    // The image decides whether the RTT control block survives a download, so
    // this must happen for every image we build, not only for one we write:
    // `preverify` can skip the write, and the DAP server builds an image it
    // has not flashed itself.
    if let Some(rtt_client) = request.rtt_client {
        ctx.object_mut(rtt_client)
            .await
            .configure_from_loader(&loader);
    }

    Ok(BuildResult {
        boot_info: convert::to_wire_boot_info(loader.boot_info()),
        loader: ctx.store_object(loader).await,
    })
}

/// Prepares the core to execute the loaded image.
///
/// When `request.resume` is true, all cores are started afterward.
pub async fn boot(ctx: &mut RpcContext, _header: VarHeader, request: BootRequest) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;

    lift(prepare_boot_info(
        &request.boot_info,
        &mut session,
        request.core_id as usize,
    ))?;
    if request.resume {
        lift(session.resume_all_cores())?;
    }

    Ok(())
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

    let loader = ctx.object_mut_blocking(request.loader);

    let mut options = flash_request_download_options(&request);
    options.dry_run = dry_run;
    options.progress = FlashProgress::new(move |event| {
        from_library_progress_event(event, |event| sender.blocking_send(event).unwrap());
    });

    lift(
        loader
            .commit(&mut session, options)
            .map_err(FileDownloadError::Flash),
    )?;

    Ok(())
}

pub async fn erase_all(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: EraseAllRequest,
) -> NoResponse {
    ctx.run_blocking::<ProgressEventTopic, _, _, _>(request, erase_all_impl)
        .await
}

fn erase_all_impl(
    ctx: RpcSpawnContext,
    request: EraseAllRequest,
    sender: Sender<ProgressEvent>,
) -> NoResponse {
    let mut session = ctx.session_blocking(request.sessid);

    let mut progress = FlashProgress::new(move |event| {
        from_library_progress_event(event, |event| {
            if event.is_operation(Operation::Erase) {
                sender.blocking_send(event).unwrap()
            }
        });
    });

    lift(flashing::erase_all(
        &mut session,
        &mut progress,
        request.read_flasher_rtt,
    ))?;

    Ok(())
}

pub async fn erase_range(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: EraseRangeRequest,
) -> NoResponse {
    ctx.run_blocking::<ProgressEventTopic, _, _, _>(request, erase_range_impl)
        .await
}

fn erase_range_impl(
    ctx: RpcSpawnContext,
    request: EraseRangeRequest,
    sender: Sender<ProgressEvent>,
) -> NoResponse {
    let mut session = ctx.session_blocking(request.sessid);

    let mut progress = FlashProgress::new(move |event| {
        from_library_progress_event(event, |event| {
            if event.is_operation(Operation::Erase) {
                sender.blocking_send(event).unwrap()
            }
        });
    });

    lift(flashing::erase(
        &mut session,
        &mut progress,
        request.address,
        request.address.saturating_add(request.length),
        request.restore,
        request.read_flasher_rtt,
    ))?;

    Ok(())
}

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

    let mut progress = FlashProgress::new(move |event| {
        from_library_progress_event(event, |event| {
            if event.is_operation(Operation::Verify)
                || matches!(event, ProgressEvent::DiagnosticMessage { .. })
            {
                sender.blocking_send(event).unwrap()
            }
        });
    });

    match loader.verify(&mut session, &mut progress) {
        Ok(()) => Ok(VerifyResult::Ok),
        Err(flashing::FlashError::Verify) => Ok(VerifyResult::Mismatch),
        Err(other) => Err(crate::rpc::functions::convert::rpc_error_flash(other)),
    }
}

pub(crate) mod convert {
    use probe_rs::flashing;
    use probe_rs_rpc::flash::{
        BootInfo, FlashDataBlockSpan, FlashFill, FlashLayout, FlashPage, FlashSector, Operation,
    };

    pub(crate) fn to_wire_flash_layout(layout: &probe_rs::flashing::FlashLayout) -> FlashLayout {
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

    pub(crate) fn to_wire_operation(operation: flashing::ProgressOperation) -> Operation {
        match operation {
            flashing::ProgressOperation::Fill => Operation::Fill,
            flashing::ProgressOperation::Erase => Operation::Erase,
            flashing::ProgressOperation::Program => Operation::Program,
            flashing::ProgressOperation::Verify => Operation::Verify,
            flashing::ProgressOperation::Ram => Operation::Ram,
        }
    }

    pub(crate) fn to_wire_boot_info(boot_info: probe_rs::flashing::BootInfo) -> BootInfo {
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

    pub(crate) fn from_wire_boot_info(boot_info: &BootInfo) -> probe_rs::flashing::BootInfo {
        match boot_info {
            BootInfo::FromRam {
                vector_table_addr,
                cores_to_reset,
            } => probe_rs::flashing::BootInfo::FromRam {
                vector_table_addr: *vector_table_addr,
                cores_to_reset: cores_to_reset.clone(),
            },
            BootInfo::Other => probe_rs::flashing::BootInfo::Other,
        }
    }
}
