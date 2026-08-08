use std::{
    any::Any,
    collections::{HashMap, HashSet},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use parking_lot::Mutex as ParkingMutex;
use probe_rs::config::Registry;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use probe_rs_rpc::{FlashLoader, Key, RttClient, Session, TempFileHandle};

pub trait ObjectMarker: 'static {
    type Object: Any + Send;
}

pub struct SessionEntry {
    pub session: probe_rs::Session,
    pub _lease: Option<probe_broker::ProbeLease>,
}

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

/// State associated with a single connection.
#[derive(Clone)]
pub struct ConnectionState {
    dry_run_sessions: Arc<ParkingMutex<HashSet<Key<Session>>>>,
    /// Generic object storage.
    object_storage: Arc<Mutex<ObjectStorage>>,
    registry: Arc<Mutex<Registry>>,
    /// Server-owned debug state (cached `DebugInfo` + per-core `VariableCache`),
    /// keyed by session. Populated by the rich stack-trace endpoint and consumed
    /// by the server-side scopes/variables endpoints.
    debug_states: Arc<Mutex<HashMap<Key<Session>, crate::rpc::debug_state::ServerDebugState>>>,
    token: CancellationToken,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            dry_run_sessions: Arc::new(ParkingMutex::new(HashSet::new())),
            object_storage: Arc::new(Mutex::new(ObjectStorage::new())),
            registry: Arc::new(Mutex::new(Registry::from_builtin_families())),
            debug_states: Arc::new(Mutex::new(HashMap::new())),
            token: CancellationToken::new(),
        }
    }

    pub async fn store_object<M: ObjectMarker>(&mut self, obj: M::Object) -> Key<M> {
        self.object_storage.lock().await.store_object(obj)
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
        dry_run: bool,
        lease: Option<probe_broker::ProbeLease>,
    ) -> Key<Session> {
        let key = self.object_storage.lock().await.store_object(SessionEntry {
            session,
            _lease: lease,
        });
        if dry_run {
            self.dry_run_sessions.lock().insert(key);
        }
        key
    }

    pub fn shared_session(&self, sid: Key<Session>) -> SessionState<'_> {
        SessionState {
            object_storage: self.object_storage.as_ref(),
            session: sid,
            dry_run: self.dry_run_sessions.lock().contains(&sid),
        }
    }
}

/// A shared handle for the [`Session`].
#[derive(Clone)]
pub struct SessionState<'a> {
    object_storage: &'a Mutex<ObjectStorage>,
    session: Key<Session>,
    dry_run: bool,
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

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }
}
