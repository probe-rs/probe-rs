use std::{
    any::Any,
    collections::HashMap,
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

pub mod client;
pub mod functions;
pub mod transport;
pub mod utils;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Hash)]
pub struct Key<T> {
    key: u64,
    marker: PhantomData<T>,
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

    #[allow(unused)]
    pub unsafe fn cast<U>(&self) -> Key<U> {
        Key {
            key: self.key,
            marker: PhantomData,
        }
    }
}

struct ObjectStorage {
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

#[derive(Clone)]
pub struct SessionState {
    dry_run: bool,
    object_storage: Arc<Mutex<ObjectStorage>>,
    registry: Arc<Mutex<Registry>>,
}

#[allow(unused)]
impl SessionState {
    pub fn new() -> Self {
        Self {
            dry_run: false,
            object_storage: Arc::new(Mutex::new(ObjectStorage::new())),
            registry: Arc::new(Mutex::new(Registry::from_builtin_families())),
        }
    }

    pub async fn store_object<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.object_storage.lock().await.store_object(obj)
    }

    pub fn store_object_blocking<T: Any + Send>(&mut self, obj: T) -> Key<T> {
        self.object_storage.blocking_lock().store_object(obj)
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
        self.dry_run = dry_run;
        key
    }

    pub fn session_blocking(
        &self,
        sid: Key<Session>,
    ) -> impl DerefMut<Target = Session> + Send + use<> {
        self.object_mut_blocking(sid)
    }

    pub fn dry_run(&self, _sid: Key<Session>) -> bool {
        self.dry_run
    }
}
