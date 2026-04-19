//! RPC-backed [`DapBackend`] implementation.
//!
//! [`RpcBackend`] proxies all session/core operations to a probe-rs RPC
//! server through [`crate::rpc::client::RpcClient`]. Because the DAP server
//! is a synchronous debugger built on top of [`probe_rs::Core`], every
//! asynchronous RPC call is bridged back to a blocking call using
//! [`tokio::runtime::Handle::block_on`] inside a [`tokio::task::block_in_place`]
//! region. The DAP session loop itself must therefore be driven from a
//! [`tokio::task::spawn_blocking`] task on a multi-threaded runtime.
//!
//! `RpcRemoteCore` is the [`probe_rs::CoreInterface`] implementation that
//! wraps the async [`crate::rpc::client::CoreInterface`] and turns each call
//! into a synchronous one. A standard [`probe_rs::Core`] handle is built by
//! [`probe_rs::Core::from_boxed`] around it.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use probe_rs::{
    Architecture, Core, CoreInformation, CoreInterface, CoreRegister, CoreRegisters, CoreStatus,
    CoreType, Endian, Error, InstructionSet, MemoryInterface, RegisterId, RegisterRole,
    RegisterValue, Session, Target, VectorCatchCondition,
};
use tokio::runtime::Handle;

use super::{DapBackend, FlashingBackend};
use crate::cmd::dap_server::DebuggerError;
use crate::cmd::dap_server::server::configuration::FlashingConfig;
use crate::rpc::{
    Key,
    client::{CoreInterface as RpcCoreClient, RpcClient, SessionInterface},
    functions::{
        core_ops::{WireCoreStatus, WireRegisterValue, WireVectorCatchCondition},
        flash::{DownloadOptions as WireDownloadOptions, ProgressEvent as WireProgressEvent, VerifyResult},
    },
};

/// Per-core cache of register values populated on demand from the bulk
/// `core/read_registers` endpoint. Shared across the short-lived
/// [`RpcRemoteCore`] instances produced by [`RpcBackend::core`] so that the
/// register dump that the DAP server performs on every halt becomes a
/// single round trip after the first register read.
type RegisterCache = Arc<Mutex<HashMap<usize, HashMap<RegisterId, RegisterValue>>>>;

/// Run an async future to completion on the current tokio runtime, without
/// actually blocking the runtime (by releasing the worker thread via
/// [`tokio::task::block_in_place`]).
fn block_on<F: std::future::Future>(handle: &Handle, fut: F) -> F::Output {
    tokio::task::block_in_place(|| handle.block_on(fut))
}

/// Convert an [`anyhow::Error`] coming out of the RPC client into the
/// [`probe_rs::Error`] surface the DAP server expects.
fn rpc_err(err: anyhow::Error) -> Error {
    Error::Other(format!("{err:?}"))
}

/// A DAP backend that drives a remote target over RPC.
pub struct RpcBackend {
    handle: Handle,
    client: RpcClient,
    sessid: Key<Session>,
    cores: Vec<(usize, CoreType)>,
    /// A real `Target` obtained from the local registry by name. The object
    /// is never used for actual probe I/O on the client side; it only needs
    /// to supply `core_index_by_address`, memory-map metadata and similar
    /// introspection that the DAP server performs locally.
    target: Arc<Target>,
    /// Per-core metadata cached at attach-time so that [`CoreInterface`]
    /// methods that expect a synchronous answer (registers, is_64_bit, ...)
    /// can be served without a round trip.
    core_metadata: Vec<CoreMetadata>,
    /// Per-core register dump cache. See [`RegisterCache`].
    register_cache: RegisterCache,
}

#[derive(Clone)]
struct CoreMetadata {
    core_type: CoreType,
    architecture: Architecture,
    endian: Endian,
    is_64_bit: bool,
    fpu_support: bool,
    fp_register_count: Option<usize>,
    registers: &'static CoreRegisters,
}

impl RpcBackend {
    /// The RPC client backing this session, used for session-level
    /// operations that are not expressible through the [`DapBackend`] trait
    /// (eg. uploading a binary and issuing a flash over the wire).
    pub(crate) fn session_interface(&self) -> SessionInterface {
        SessionInterface::new(self.client.clone(), self.sessid)
    }

    /// Access the tokio runtime handle used to drive async RPC calls from a
    /// synchronous [`CoreInterface`] context.
    #[allow(
        dead_code,
        reason = "Kept as a symmetric accessor alongside `session_interface`; consumed by future iterations of the backend glue."
    )]
    pub(crate) fn tokio_handle(&self) -> Handle {
        self.handle.clone()
    }
}

#[allow(
    dead_code,
    reason = "new/session_interface helpers keep being invoked from later patches."
)]
impl RpcBackend {
    /// Build a new RPC backend.
    ///
    /// The caller is responsible for:
    /// * having already completed `probe/attach` over RPC (yielding a
    ///   `Key<Session>`),
    /// * producing a matching [`Target`] from the local chip registry (so
    ///   that memory-map and core-addressing introspection works without
    ///   extra round trips),
    /// * supplying per-core metadata: either by querying the server at
    ///   attach-time or by inferring it from the target description.
    pub fn new(
        handle: Handle,
        client: RpcClient,
        sessid: Key<Session>,
        target: Target,
        cores: Vec<(usize, CoreType)>,
        per_core: Vec<CorePerAttachInfo>,
    ) -> Self {
        let core_metadata = cores
            .iter()
            .zip(per_core)
            .map(|((_, core_type), info)| {
                let registers = CoreRegisters::for_core_type(
                    *core_type,
                    info.fpu_support,
                    info.fp_register_count,
                );
                CoreMetadata {
                    core_type: *core_type,
                    architecture: info.architecture,
                    endian: info.endian,
                    is_64_bit: info.is_64_bit,
                    fpu_support: info.fpu_support,
                    fp_register_count: info.fp_register_count,
                    registers,
                }
            })
            .collect();

        Self {
            handle,
            client,
            sessid,
            cores,
            target: Arc::new(target),
            core_metadata,
            register_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Per-core information the [`RpcBackend`] caller has to gather at attach
/// time. Most of these are static properties of the core type once the
/// target has booted, so a single query is enough.
#[derive(Clone, Copy)]
pub struct CorePerAttachInfo {
    pub architecture: Architecture,
    pub endian: Endian,
    pub is_64_bit: bool,
    pub fpu_support: bool,
    pub fp_register_count: Option<usize>,
}

impl DapBackend for RpcBackend {
    fn list_cores(&self) -> Vec<(usize, CoreType)> {
        self.cores.clone()
    }

    fn target(&self) -> &Target {
        &self.target
    }

    fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error> {
        let metadata = self
            .cores
            .iter()
            .zip(self.core_metadata.iter())
            .find_map(|((idx, _), meta)| (*idx == core_index).then_some(meta.clone()))
            .ok_or(Error::CoreNotFound(core_index))?;

        let core = RpcRemoteCore {
            handle: self.handle.clone(),
            client: RpcCoreClient::new_for_backend(
                self.client.clone(),
                self.sessid,
                core_index as u32,
            ),
            metadata,
            core_index,
            register_cache: self.register_cache.clone(),
        };

        // The `Core` wraps a `Box<dyn CoreInterface + 'probe>`; we borrow the
        // target description from `self` so that DAP code paths that ask for
        // `core.target()` keep working.
        let target: &Target = &self.target;
        let name: &str = &self.target.name;
        Ok(Core::from_boxed(core_index, name, target, Box::new(core)))
    }
}

/// Synchronous [`CoreInterface`] implementation backed by an async RPC client.
pub struct RpcRemoteCore {
    handle: Handle,
    client: RpcCoreClient,
    metadata: CoreMetadata,
    core_index: usize,
    register_cache: RegisterCache,
}

impl RpcRemoteCore {
    /// Invalidate this core's cached register dump. Called whenever an
    /// operation is issued that could plausibly change register contents:
    /// `run`, `step`, `halt`, any reset, or a register write.
    fn invalidate_register_cache(&self) {
        if let Ok(mut cache) = self.register_cache.lock() {
            cache.remove(&self.core_index);
        }
    }

    /// Look up a single register, refilling the cache with a batched
    /// `core/read_registers` call on a miss.
    fn cached_read_reg(&mut self, id: RegisterId) -> Result<RegisterValue, Error> {
        if let Ok(cache) = self.register_cache.lock()
            && let Some(entry) = cache.get(&self.core_index)
            && let Some(value) = entry.get(&id)
        {
            return Ok(*value);
        }

        self.refill_register_cache()?;

        if let Ok(cache) = self.register_cache.lock()
            && let Some(entry) = cache.get(&self.core_index)
            && let Some(value) = entry.get(&id)
        {
            return Ok(*value);
        }

        // The batched read did not return the requested register (either
        // the target refused to read it, or it is not part of the static
        // register file). Fall back to a direct single read so the caller
        // still gets an authoritative answer / error.
        let wire: WireRegisterValue =
            block_on(&self.handle, self.client.read_core_reg(id.into())).map_err(rpc_err)?;
        Ok(wire.into())
    }

    /// Issue a single `core/read_registers` call covering every register in
    /// this core's static register file (including FP registers when
    /// available) and populate the cache with whatever the server returns.
    fn refill_register_cache(&self) -> Result<(), Error> {
        let mut ids: Vec<RegisterId> = self
            .metadata
            .registers
            .core_registers()
            .map(|r| r.id())
            .collect();
        if self.metadata.fpu_support
            && let Some(fpu) = self.metadata.registers.fpu_registers()
        {
            ids.extend(fpu.map(|r| r.id()));
        }
        let wire_ids = ids.iter().copied().map(Into::into).collect();

        let results =
            block_on(&self.handle, self.client.read_registers(wire_ids)).map_err(rpc_err)?;

        let mut cache = self
            .register_cache
            .lock()
            .map_err(|_| Error::Other("register cache poisoned".to_string()))?;
        let entry = cache.entry(self.core_index).or_default();
        for result in results {
            if let Some(value) = result.value {
                entry.insert(result.id.into(), value.into());
            }
        }
        Ok(())
    }
}

/// Helper that resolves a single [`CoreRegister`] from the static register
/// table by role, or panics with a descriptive message if the target is
/// misconfigured.
///
/// The panic on a missing register mirrors [`probe_rs::Core`]'s own
/// assumption that every supported target has a stack pointer / frame
/// pointer / return address / program counter in its register file.
#[allow(
    clippy::panic,
    reason = "mirrors probe_rs::Core's invariants about the register file"
)]
fn register_with_role(
    registers: &'static CoreRegisters,
    role: RegisterRole,
    name: &'static str,
) -> &'static CoreRegister {
    registers
        .core_registers()
        .find(|r| r.register_has_role(role))
        .unwrap_or_else(|| panic!("register set is missing the {name} register"))
}

impl MemoryInterface for RpcRemoteCore {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.metadata.is_64_bit
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        let data =
            block_on(&self.handle, self.client.read_memory_8(address, 1)).map_err(rpc_err)?;
        data.into_iter()
            .next()
            .ok_or_else(|| Error::Other("empty response from memory/read8".to_string()))
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        let data =
            block_on(&self.handle, self.client.read_memory_16(address, 1)).map_err(rpc_err)?;
        data.into_iter()
            .next()
            .ok_or_else(|| Error::Other("empty response from memory/read16".to_string()))
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        let data =
            block_on(&self.handle, self.client.read_memory_32(address, 1)).map_err(rpc_err)?;
        data.into_iter()
            .next()
            .ok_or_else(|| Error::Other("empty response from memory/read32".to_string()))
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        let data =
            block_on(&self.handle, self.client.read_memory_64(address, 1)).map_err(rpc_err)?;
        data.into_iter()
            .next()
            .ok_or_else(|| Error::Other("empty response from memory/read64".to_string()))
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        let result = block_on(&self.handle, self.client.read_memory_8(address, data.len()))
            .map_err(rpc_err)?;
        if result.len() != data.len() {
            return Err(Error::Other(format!(
                "short read: requested {} bytes, got {}",
                data.len(),
                result.len()
            )));
        }
        data.copy_from_slice(&result);
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        let result = block_on(
            &self.handle,
            self.client.read_memory_16(address, data.len()),
        )
        .map_err(rpc_err)?;
        if result.len() != data.len() {
            return Err(Error::Other(format!(
                "short read: requested {} words, got {}",
                data.len(),
                result.len()
            )));
        }
        data.copy_from_slice(&result);
        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        let result = block_on(
            &self.handle,
            self.client.read_memory_32(address, data.len()),
        )
        .map_err(rpc_err)?;
        if result.len() != data.len() {
            return Err(Error::Other(format!(
                "short read: requested {} words, got {}",
                data.len(),
                result.len()
            )));
        }
        data.copy_from_slice(&result);
        Ok(())
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        let result = block_on(
            &self.handle,
            self.client.read_memory_64(address, data.len()),
        )
        .map_err(rpc_err)?;
        if result.len() != data.len() {
            return Err(Error::Other(format!(
                "short read: requested {} words, got {}",
                data.len(),
                result.len()
            )));
        }
        data.copy_from_slice(&result);
        Ok(())
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_8(address, vec![data]),
        )
        .map_err(rpc_err)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_16(address, vec![data]),
        )
        .map_err(rpc_err)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_32(address, vec![data]),
        )
        .map_err(rpc_err)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_64(address, vec![data]),
        )
        .map_err(rpc_err)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_8(address, data.to_vec()),
        )
        .map_err(rpc_err)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_16(address, data.to_vec()),
        )
        .map_err(rpc_err)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_32(address, data.to_vec()),
        )
        .map_err(rpc_err)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        block_on(
            &self.handle,
            self.client.write_memory_64(address, data.to_vec()),
        )
        .map_err(rpc_err)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl CoreInterface for RpcRemoteCore {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        block_on(&self.handle, self.client.wait_for_core_halted(timeout)).map_err(rpc_err)
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        block_on(&self.handle, self.client.core_halted()).map_err(rpc_err)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let wire: WireCoreStatus = block_on(&self.handle, self.client.status()).map_err(rpc_err)?;
        Ok(wire.into())
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.invalidate_register_cache();
        let info = block_on(&self.handle, self.client.halt(timeout)).map_err(rpc_err)?;
        Ok(info.into())
    }

    fn run(&mut self) -> Result<(), Error> {
        self.invalidate_register_cache();
        block_on(&self.handle, self.client.run()).map_err(rpc_err)
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.invalidate_register_cache();
        block_on(&self.handle, self.client.reset()).map_err(rpc_err)
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.invalidate_register_cache();
        block_on(&self.handle, self.client.reset_and_halt(timeout)).map_err(rpc_err)?;
        // The existing `reset_and_halt` endpoint only returns `()`; the PC
        // will be read by the next call anyway. Surface a zero-filled
        // `CoreInformation` until a richer endpoint is wired up.
        Ok(CoreInformation { pc: 0 })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.invalidate_register_cache();
        let info = block_on(&self.handle, self.client.step()).map_err(rpc_err)?;
        Ok(info.into())
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        self.cached_read_reg(address)
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        self.invalidate_register_cache();
        block_on(
            &self.handle,
            self.client.write_core_reg(address.into(), value.into()),
        )
        .map_err(rpc_err)
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        block_on(&self.handle, self.client.available_breakpoint_units()).map_err(rpc_err)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        // The current RPC surface exposes breakpoint management as
        // address-based set/clear operations (the server performs unit
        // allocation). We therefore do not expose the raw breakpoint unit
        // table. `Core::set_hw_breakpoint(addr)` must not be used on a
        // remote core; callers should invoke `set_hw_breakpoint` directly
        // on this `CoreInterface` and pass `0` for the unit index.
        Err(Error::NotImplemented(
            "hw_breakpoints over RPC; use set_hw_breakpoint(addr) directly",
        ))
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        // Breakpoints are enabled implicitly by the server when
        // `core/set_hw_bp` is invoked.
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, _unit_index: usize, addr: u64) -> Result<(), Error> {
        // `unit_index` is ignored: server-side `core/set_hw_bp` performs its
        // own allocation.
        block_on(&self.handle, self.client.set_hw_breakpoint(addr)).map_err(rpc_err)
    }

    fn clear_hw_breakpoint(&mut self, _unit_index: usize) -> Result<(), Error> {
        // With the address-based endpoint we cannot clear by unit index
        // alone. DAP code paths that reach this trait method go through
        // `Core::clear_hw_breakpoint(addr)`, which resolves the address
        // first via `hw_breakpoints()` - but we returned `NotImplemented`
        // from that, so callers must use the address-based path instead.
        Err(Error::NotImplemented(
            "clear_hw_breakpoint by unit index; use the address-based path",
        ))
    }

    fn registers(&self) -> &'static CoreRegisters {
        self.metadata.registers
    }

    fn program_counter(&self) -> &'static CoreRegister {
        #[allow(
            clippy::expect_used,
            reason = "mirrors probe_rs::Core's invariant that every supported core has a PC"
        )]
        self.metadata
            .registers
            .pc()
            .expect("register set must contain a program counter")
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        register_with_role(
            self.metadata.registers,
            RegisterRole::FramePointer,
            "frame pointer",
        )
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        register_with_role(
            self.metadata.registers,
            RegisterRole::StackPointer,
            "stack pointer",
        )
    }

    fn return_address(&self) -> &'static CoreRegister {
        register_with_role(
            self.metadata.registers,
            RegisterRole::ReturnAddress,
            "return address",
        )
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        true
    }

    fn architecture(&self) -> Architecture {
        self.metadata.architecture
    }

    fn core_type(&self) -> CoreType {
        self.metadata.core_type
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        let wire = block_on(&self.handle, self.client.instruction_set()).map_err(rpc_err)?;
        Ok(wire.into())
    }

    fn endianness(&mut self) -> Result<Endian, Error> {
        Ok(self.metadata.endian)
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        Ok(self.metadata.fpu_support)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(self.metadata.fp_register_count.unwrap_or(0))
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        // Reset-catch is not currently exposed over RPC.
        Err(Error::NotImplemented("reset_catch_set over RPC"))
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("reset_catch_clear over RPC"))
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        let wire: WireVectorCatchCondition = condition.into();
        block_on(&self.handle, self.client.enable_vector_catch(wire)).map_err(rpc_err)
    }

    fn disable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        let wire: WireVectorCatchCondition = condition.into();
        block_on(&self.handle, self.client.disable_vector_catch(wire)).map_err(rpc_err)
    }

    fn is_64_bit(&self) -> bool {
        self.metadata.is_64_bit
    }
}

impl FlashingBackend for RpcBackend {
    async fn flash_binary(
        &mut self,
        path_to_elf: &Path,
        config: &FlashingConfig,
        progress: &mut dyn FnMut(WireProgressEvent),
    ) -> Result<(), DebuggerError> {
        let session = self.session_interface();

        let build_result = session
            .build_flash_loader(
                path_to_elf.to_path_buf(),
                config.format_options.clone(),
                None,
                false,
            )
            .await
            .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?;

        let loader_key = build_result.loader;

        let run_flash = if config.verify_before_flashing {
            match session
                .verify(loader_key, async |event| {
                    progress(event);
                })
                .await
                .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?
            {
                VerifyResult::Ok => false,
                VerifyResult::Mismatch => true,
            }
        } else {
            true
        };

        if run_flash {
            let options = WireDownloadOptions {
                keep_unwritten_bytes: config.restore_unwritten_bytes,
                do_chip_erase: config.full_chip_erase,
                skip_erase: false,
                verify: config.verify_after_flashing,
                disable_double_buffering: false,
                preferred_algos: Vec::new(),
            };

            session
                .flash(options, loader_key, None, async |event| {
                    progress(event);
                })
                .await
                .map_err(|e| DebuggerError::Other(anyhow::anyhow!(e)))?;
        }

        Ok(())
    }
}
