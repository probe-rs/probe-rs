use crate::rpc::{
    Key,
    functions::{NoResponse, RpcContext, RpcResult},
};
use postcard_rpc::header::VarHeader;
use postcard_schema::Schema;
use probe_rs::{MemoryInterface, Session};
use serde::{Deserialize, Serialize};

pub trait Word: Copy + Default + Send + Schema {
    async fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()>;

    async fn write(
        core: &mut impl MemoryInterface,
        address: u64,
        data: &[Self],
    ) -> anyhow::Result<()>;
}

impl Word for u8 {
    async fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_8(address, out).await?;
        Ok(())
    }

    async fn write(
        core: &mut impl MemoryInterface,
        address: u64,
        data: &[Self],
    ) -> anyhow::Result<()> {
        core.write_8(address, data).await?;
        Ok(())
    }
}
impl Word for u16 {
    async fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_16(address, out).await?;
        Ok(())
    }

    async fn write(
        core: &mut impl MemoryInterface,
        address: u64,
        data: &[Self],
    ) -> anyhow::Result<()> {
        core.write_16(address, data).await?;
        Ok(())
    }
}
impl Word for u32 {
    async fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_32(address, out).await?;
        Ok(())
    }

    async fn write(
        core: &mut impl MemoryInterface,
        address: u64,
        data: &[Self],
    ) -> anyhow::Result<()> {
        core.write_32(address, data).await?;
        Ok(())
    }
}
impl Word for u64 {
    async fn read(
        core: &mut impl MemoryInterface,
        address: u64,
        out: &mut Vec<Self>,
    ) -> anyhow::Result<()> {
        core.read_64(address, out).await?;
        Ok(())
    }

    async fn write(
        core: &mut impl MemoryInterface,
        address: u64,
        data: &[Self],
    ) -> anyhow::Result<()> {
        core.write_64(address, data).await?;
        Ok(())
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
    let mut core = session.core(request.core as usize).await.unwrap();
    W::write(&mut core, request.address, &request.data).await?;
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
    let mut core = session.core(request.core as usize).await?;

    let mut words = vec![W::default(); request.count as usize];
    W::read(&mut core, request.address, &mut words).await?;
    Ok(words)
}
