use std::{
    hash::{Hash, Hasher},
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

use postcard_rpc::server::WireTxErrorKind;
use postcard_schema::{
    Schema,
    schema::{DataModelType, NamedType, NamedValue},
};
use serde::{Deserialize, Serialize};

pub struct Session;
pub struct FlashLoader;
pub struct RttClient;
pub struct TempFileHandle;

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
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self {
            key: COUNTER.fetch_add(1, Ordering::Relaxed),
            marker: PhantomData,
        }
    }

    pub fn id(&self) -> u64 {
        self.key
    }

    /// Test helper for constructing a [`Key`] with a fixed id.
    pub fn test(id: u64) -> Self {
        Self {
            key: id,
            marker: PhantomData,
        }
    }
}

impl<T> Default for Key<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct RpcError(String);

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for RpcError {
    fn from(e: &str) -> Self {
        Self(e.to_string())
    }
}

impl From<String> for RpcError {
    fn from(e: String) -> Self {
        Self(e)
    }
}

impl From<WireTxErrorKind> for RpcError {
    fn from(e: WireTxErrorKind) -> Self {
        Self(format!("{e:?}"))
    }
}

pub type RpcResult<T> = Result<T, RpcError>;

pub type NoResponse = RpcResult<()>;
