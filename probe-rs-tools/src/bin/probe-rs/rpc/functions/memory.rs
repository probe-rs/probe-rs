use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext, RpcResult},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{MemoryInterface, Session};
use serde::{Deserialize, Serialize};

pub trait Word: Copy + Default + Send + Schema {
    fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()>;

    fn write(core: &mut impl MemoryInterface, address: u64, data: &[Self]) -> anyhow::Result<()>;

    /// Size of this word type in bytes.
    fn byte_size() -> usize;

    /// Convert bytes to words (bytes are in little-endian order).
    fn from_bytes(bytes: &[u8]) -> Vec<Self>;
}

impl Word for u8 {
    fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_8(address, out)?;
        Ok(())
    }

    fn write(core: &mut impl MemoryInterface, address: u64, data: &[Self]) -> anyhow::Result<()> {
        core.write_8(address, data)?;
        Ok(())
    }

    fn byte_size() -> usize {
        1
    }

    fn from_bytes(bytes: &[u8]) -> Vec<Self> {
        bytes.to_vec()
    }
}
impl Word for u16 {
    fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_16(address, out)?;
        Ok(())
    }

    fn write(core: &mut impl MemoryInterface, address: u64, data: &[Self]) -> anyhow::Result<()> {
        core.write_16(address, data)?;
        Ok(())
    }

    fn byte_size() -> usize {
        2
    }

    fn from_bytes(bytes: &[u8]) -> Vec<Self> {
        bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }
}
impl Word for u32 {
    fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_32(address, out)?;
        Ok(())
    }

    fn write(core: &mut impl MemoryInterface, address: u64, data: &[Self]) -> anyhow::Result<()> {
        core.write_32(address, data)?;
        Ok(())
    }

    fn byte_size() -> usize {
        4
    }

    fn from_bytes(bytes: &[u8]) -> Vec<Self> {
        bytes
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
}
impl Word for u64 {
    fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_64(address, out)?;
        Ok(())
    }

    fn write(core: &mut impl MemoryInterface, address: u64, data: &[Self]) -> anyhow::Result<()> {
        core.write_64(address, data)?;
        Ok(())
    }

    fn byte_size() -> usize {
        8
    }

    fn from_bytes(bytes: &[u8]) -> Vec<Self> {
        bytes
            .chunks(8)
            .map(|c| {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(c);
                u64::from_le_bytes(arr)
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize, Schema)]
pub struct WriteMemoryRequest<W: Word> {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
    pub data: Vec<W>,
}

pub async fn write_memory<W: Word>(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: WriteMemoryRequest<W>,
) -> NoResponse {
    let mut session = ctx.session(request.sessid).await;
    let mut core = session.core(request.core as usize).unwrap();
    W::write(&mut core, request.address, &request.data)?;
    Ok(())
}

#[derive(Serialize, Deserialize, Schema)]
pub struct ReadMemoryRequest {
    pub sessid: Key<Session>,
    pub core: u32,
    pub address: u64,
    pub count: u32,
}

pub async fn read_memory<W: Word>(
    ctx: &mut RpcContext,
    _header: VarHeader,
    request: ReadMemoryRequest,
) -> RpcResult<Vec<W>> {
    let mut session = ctx.session(request.sessid).await;

    // Try probe-assisted read for faster flash access
    let byte_len = request.count as usize * W::byte_size();
    if let Ok(Some(data)) =
        session.try_read_flash_via_probe(request.address as u32, byte_len as u32)
    {
        if data.len() == byte_len {
            return Ok(W::from_bytes(&data));
        }
    }

    let mut core = session.core(request.core as usize)?;

    let mut words = vec![W::default(); request.count as usize];
    W::read(&mut core, request.address, &mut words)?;
    Ok(words)
}
