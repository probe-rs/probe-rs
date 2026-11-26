use std::{
    any::Any,
    collections::{HashMap, HashSet},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use probe_rs::config::Registry;
use probe_rs_rpc::host_calls::HostReply;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::rpc::functions::{rtt_client::RttDownStream, write_queue::Streams};

pub use probe_rs_rpc::{FlashLoader, Key, RttClient, Session, TempFileHandle};

pub trait ObjectMarker: 'static {
    type Object: Any + Send;
}

pub struct SessionEntry {
    pub session: probe_rs::Session,
    // Fields drop in order. The serial port must close before the lease hands the probe to
    // the next client, which opens the port again.
    pub _target_serial: Option<SharedTargetSerial>,
    pub _lease: probe_broker::ProbeLease,
}

/// A serial port to the target that more than one task uses.
pub type SharedTargetSerial = Arc<dyn probe_rs::probe::target_serial::TargetSerial>;
type WeakTargetSerial = Weak<dyn probe_rs::probe::target_serial::TargetSerial>;

impl Deref for SessionEntry {
    type Target = probe_rs::Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl DerefMut for SessionEntry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

impl ObjectMarker for Session {
    type Object = SessionEntry;
}

impl ObjectMarker for FlashLoader {
    type Object = probe_rs::flashing::FlashLoader;
}

impl ObjectMarker for RttClient {
    type Object = crate::util::rtt::client::RttClient;
}

impl ObjectMarker for TempFileHandle {
    type Object = tempfile::NamedTempFile;
}

pub mod debug_state;
pub mod functions;
pub mod probe_broker;
pub mod svd;
pub mod utils;

#[cfg(test)]
mod client_tests;

pub(crate) struct ObjectStorage {
    storage: HashMap<u64, Arc<Mutex<dyn Any + Send>>>,
}

pub(crate) struct ObjectStorageSlot<T: Any + Send> {
    obj: Arc<Mutex<dyn Any + Send>>,
    _type: PhantomData<fn() -> T>,
}

impl<T: Any + Send> ObjectStorageSlot<T> {
    pub async fn get(&self) -> impl DerefMut<Target = T> + Send + use<T> {
        let guard = self.obj.clone().lock_owned().await;
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            e.downcast_mut::<T>().unwrap()
        })
    }

    /// Blocking variant of [`ObjectStorageSlot::get`]; only use in synchronous contexts.
    pub fn get_blocking(&self) -> impl DerefMut<Target = T> + Send + use<T> {
        let guard = self.obj.clone().blocking_lock_owned();
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            e.downcast_mut::<T>().unwrap()
        })
    }
}

impl ObjectStorage {
    fn new() -> Self {
        Self {
            storage: HashMap::new(),
        }
    }

    pub fn store_object<M: ObjectMarker>(&mut self, obj: M::Object) -> Key<M> {
        let key = Key::new();
        self.storage.insert(key.id(), Arc::new(Mutex::new(obj)));
        key
    }

    /// Ensures locks on `ObjectStorage` are held for as short a time as possible.
    pub fn cell<M: ObjectMarker>(&self, key: Key<M>) -> ObjectStorageSlot<M::Object> {
        let obj = self.storage.get(&key.id()).unwrap();
        ObjectStorageSlot {
            obj: obj.clone(),
            _type: PhantomData,
        }
    }
}

struct HostReplySlot {
    /// The first call of the current monitor or test call.
    first_id: u64,
    sender: std::sync::mpsc::SyncSender<HostReply>,
}

/// Receives the replies to host calls. Dropping it stops the delivery.
pub struct HostReplies {
    slot: Arc<parking_lot::Mutex<Option<HostReplySlot>>>,
    next_id: Arc<AtomicU64>,
    receiver: std::sync::mpsc::Receiver<HostReply>,
}

impl HostReplies {
    pub fn receiver(&self) -> &std::sync::mpsc::Receiver<HostReply> {
        &self.receiver
    }

    /// Returns the id for the next call. It is unique on the connection.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl Drop for HostReplies {
    fn drop(&mut self) {
        *self.slot.lock() = None;
    }
}

/// State associated with a single connection.
#[derive(Clone)]
pub struct ConnectionState {
    /// Generic object storage.
    object_storage: Arc<Mutex<ObjectStorage>>,
    registry: Arc<Mutex<Registry>>,
    /// Server-owned debug state (cached `DebugInfo` + per-core `VariableCache`),
    /// keyed by session. Populated by the rich stack-trace endpoint and consumed
    /// by the server-side scopes/variables endpoints.
    debug_states: Arc<Mutex<HashMap<Key<Session>, crate::rpc::debug_state::ServerDebugState>>>,
    /// The serial ports to the target, keyed by session. They are here as well as in the
    /// session, so that their users do not wait for the session lock. The session owns them.
    target_serials: Arc<parking_lot::Mutex<HashMap<Key<Session>, WeakTargetSerial>>>,
    /// Where a reply to a semihosting host call goes.
    host_replies: Arc<parking_lot::Mutex<Option<HostReplySlot>>>,
    next_host_call_id: Arc<AtomicU64>,
    /// The paths of the temporary files that this connection created.
    uploads: Arc<parking_lot::Mutex<HashSet<String>>>,
    /// The RTT down channels that run loops write, keyed by RTT client.
    rtt_down: Streams<Key<RttClient>, RttDownStream>,
    /// The write queues of the serial ports that run loops write, keyed by session.
    serial_writes: Streams<Key<Session>, mpsc::Sender<Vec<u8>>>,
    token: CancellationToken,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            object_storage: Arc::new(Mutex::new(ObjectStorage::new())),
            registry: Arc::new(Mutex::new(Registry::from_builtin_families())),
            debug_states: Arc::new(Mutex::new(HashMap::new())),
            target_serials: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            host_replies: Arc::new(parking_lot::Mutex::new(None)),
            next_host_call_id: Arc::new(AtomicU64::new(0)),
            uploads: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            rtt_down: Streams::new(),
            serial_writes: Streams::new(),
            token: CancellationToken::new(),
        }
    }

    pub async fn store_object<M: ObjectMarker>(&mut self, obj: M::Object) -> Key<M> {
        self.object_storage.lock().await.store_object(obj)
    }

    pub async fn session(
        &self,
        sid: Key<Session>,
    ) -> impl DerefMut<Target = probe_rs::Session> + Send + use<> {
        // MUST be two separate statements so that the lock is released.
        let locked_cell = self.object_storage.lock().await.cell(sid);
        let guard = locked_cell.obj.clone().lock_owned().await;
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            &mut e.downcast_mut::<SessionEntry>().unwrap().session
        })
    }

    pub async fn object_mut<M: ObjectMarker>(
        &self,
        key: Key<M>,
    ) -> impl DerefMut<Target = M::Object> + Send + use<M> {
        // MUST be two separate statements so that the lock is released.
        let locked_cell = self.object_storage.lock().await.cell(key);
        locked_cell.get().await
    }

    pub fn object_mut_blocking<M: ObjectMarker>(
        &self,
        key: Key<M>,
    ) -> impl DerefMut<Target = M::Object> + Send + use<M> {
        // MUST be two separate statements so that the lock is released.
        let locked_cell = self.object_storage.blocking_lock().cell(key);
        locked_cell.get_blocking()
    }

    pub async fn set_session(
        &self,
        session: probe_rs::Session,
        target_serial: Option<SharedTargetSerial>,
        lease: probe_broker::ProbeLease,
    ) -> Key<Session> {
        let weak_serial = target_serial.as_ref().map(Arc::downgrade);
        let sid = self.object_storage.lock().await.store_object(SessionEntry {
            session,
            _target_serial: target_serial,
            _lease: lease,
        });
        if let Some(weak_serial) = weak_serial {
            self.target_serials.lock().insert(sid, weak_serial);
        }
        sid
    }

    /// Returns the serial port to the target of a session.
    pub fn target_serial(&self, sid: Key<Session>) -> Option<SharedTargetSerial> {
        self.target_serials.lock().get(&sid)?.upgrade()
    }

    /// Starts to accept replies to host calls, for the duration of a monitor or test call.
    ///
    /// A connection has one client that answers host calls, so only one call at a time can use
    /// them.
    pub fn host_replies(&self) -> anyhow::Result<HostReplies> {
        let mut slot = self.host_replies.lock();
        if slot.is_some() {
            anyhow::bail!("Another monitor or test call of this connection uses semihosting files");
        }
        // A full channel drops a reply, so it holds every reply that the client can owe. Room for as
        // many more stops stale replies from taking their place.
        let (sender, receiver) =
            std::sync::mpsc::sync_channel(2 * crate::rpc::utils::semihosting::MAX_OPEN_REPLIES);
        *slot = Some(HostReplySlot {
            first_id: self.next_host_call_id.load(Ordering::Relaxed),
            sender,
        });
        Ok(HostReplies {
            slot: self.host_replies.clone(),
            next_id: self.next_host_call_id.clone(),
            receiver,
        })
    }

    /// Passes a reply to the running monitor or test call. This never blocks. A reply to an earlier
    /// call is stale, for example from a test that timed out.
    pub fn deliver_host_reply(&self, reply: HostReply) {
        if let Some(slot) = &*self.host_replies.lock()
            && reply.id >= slot.first_id
        {
            _ = slot.sender.try_send(reply);
        }
    }

    /// Records the path of a temporary file that this connection created.
    pub fn add_upload(&self, path: String) {
        self.uploads.lock().insert(path);
    }

    /// Returns `true` if this connection created the temporary file at `path`.
    pub fn is_upload(&self, path: &str) -> bool {
        self.uploads.lock().contains(path)
    }

    pub fn shared_session(&self, sid: Key<Session>) -> SessionState<'_> {
        SessionState {
            object_storage: self.object_storage.as_ref(),
            session: sid,
        }
    }
}

/// A shared handle for the [`Session`].
#[derive(Clone)]
pub struct SessionState<'a> {
    object_storage: &'a Mutex<ObjectStorage>,
    session: Key<Session>,
}

impl SessionState<'_> {
    /// Blocks while other users hold the underlying storage.
    pub fn object_storage(&self) -> impl DerefMut<Target = ObjectStorage> + Send + use<'_> {
        self.object_storage.blocking_lock()
    }

    /// Blocks while other users hold the session.
    pub fn session_blocking(&self) -> impl DerefMut<Target = probe_rs::Session> + Send + use<> {
        let obj_cell = self.object_storage().cell(self.session);
        let guard = obj_cell.obj.clone().blocking_lock_owned();
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            &mut e.downcast_mut::<SessionEntry>().unwrap().session
        })
    }
}
