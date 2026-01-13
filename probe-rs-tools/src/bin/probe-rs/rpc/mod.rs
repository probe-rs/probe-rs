use std::{
    any::Any,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::DerefMut,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use postcard_schema::{
    Schema,
    schema::{DataModelType, NamedType, NamedValue},
};
use probe_rs::{Session, config::Registry};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub mod client;
pub mod functions;
pub mod transport;
pub mod utils;

#[derive(Serialize, Deserialize, Debug)]
pub struct Key<T> {
    key: u64,
    marker: PhantomData<T>,
}

impl<T> Eq for Key<T> {}
impl<T> PartialEq for Key<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl<T> Hash for Key<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

unsafe impl<T> Send for Key<T> {}
unsafe impl<T> Sync for Key<T> {}

impl<T> Schema for Key<T> {
    const SCHEMA: &'static NamedType = &NamedType {
        name: "Key<T>",
        ty: &DataModelType::Struct(&[
            &NamedValue {
                name: "key",
                ty: &NamedType {
                    name: "u64",
                    ty: &DataModelType::U64,
                },
            },
            &NamedValue {
                name: "marker",
                ty: &NamedType {
                    name: "PhantomData<T>",
                    ty: &DataModelType::UnitStruct,
                },
            },
        ]),
    };
}

impl<T> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Key<T> {}

impl<T> Key<T> {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self {
            key: COUNTER.fetch_add(1, Ordering::Relaxed),
            marker: PhantomData,
        }
    }

    #[cfg(feature = "remote")]
    pub unsafe fn cast<U>(&self) -> Key<U> {
        Key {
            key: self.key,
            marker: PhantomData,
        }
    }
}

pub(crate) struct ObjectStorage {
    storage: HashMap<u64, Arc<Mutex<dyn Any + Send>>>,
}

impl ObjectStorage {
    fn new() -> Self {
        Self {
            storage: HashMap::new(),
        }
    }

    pub fn store_object<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        let key = Key::new();
        self.storage.insert(key.key, Arc::new(Mutex::new(obj)));
        key
    }

    pub async fn object_mut<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        let obj = self.storage.get(&key.key).unwrap();
        let guard = obj.clone().lock_owned().await;
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            e.downcast_mut::<T>().unwrap()
        })
    }

    pub fn object_mut_blocking<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        let obj = self.storage.get(&key.key).unwrap();
        let guard = obj.clone().blocking_lock_owned();
        tokio::sync::OwnedMutexGuard::map(guard, |e: &mut (dyn Any + Send)| {
            e.downcast_mut::<T>().unwrap()
        })
    }
}

/// State associated with a single connection.
#[derive(Clone)]
pub struct ConnectionState {
    dry_run_sessions: HashSet<Key<Session>>,
    /// Generic object storage.
    object_storage: Arc<Mutex<ObjectStorage>>,
    registry: Arc<Mutex<Registry>>,
    token: CancellationToken,
}

impl ConnectionState {
    pub fn new() -> Self {
        Self {
            dry_run_sessions: HashSet::new(),
            object_storage: Arc::new(Mutex::new(ObjectStorage::new())),
            registry: Arc::new(Mutex::new(Registry::from_builtin_families())),
            token: CancellationToken::new(),
        }
    }

    pub async fn store_object<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.object_storage.lock().await.store_object(obj)
    }

    pub async fn object_mut<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        self.object_storage.lock().await.object_mut(key).await
    }

    pub fn object_mut_blocking<T: Any + Send>(
        &self,
        key: Key<T>,
    ) -> impl DerefMut<Target = T> + Send + use<T> {
        self.object_storage.blocking_lock().object_mut_blocking(key)
    }

    pub async fn set_session(&mut self, session: Session, dry_run: bool) -> Key<Session> {
        let key = self.store_object(session).await;
        if dry_run {
            self.dry_run_sessions.insert(key);
        }
        key
    }

    pub fn shared_session(&self, sid: Key<Session>) -> SessionState<'_> {
        SessionState {
            object_storage: self.object_storage.as_ref(),
            session: sid,
            dry_run: self.dry_run_sessions.contains(&sid),
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
    /// Returns a handle to the session.
    ///
    /// This function blocks while other users hold the session.
    pub fn object_storage(&self) -> impl DerefMut<Target = ObjectStorage> + Send + use<'_> {
        self.object_storage.blocking_lock()
    }

    /// Returns a handle to the session.
    ///
    /// This function blocks while other users hold the session.
    pub fn session_blocking(&self) -> impl DerefMut<Target = Session> + Send + use<> {
        self.object_storage().object_mut_blocking(self.session)
    }

    /// Returns whether the session is in dry-run mode.
    pub fn dry_run(&self) -> bool {
        self.dry_run
    }
}
