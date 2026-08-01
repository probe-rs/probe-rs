use postcard_rpc::server::{WireRxErrorKind, WireTxErrorKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::Mutex;

use crate::transport::{
    LENGTH_PREFIX_LEN, frame,
    memory::{PostcardReceiver, PostcardSender},
};

pub struct UnixStreamTx {
    writer: Mutex<OwnedWriteHalf>,
}

impl UnixStreamTx {
    pub fn new(writer: OwnedWriteHalf) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }
}

impl PostcardSender for UnixStreamTx {
    async fn send(&self, msg: Vec<u8>) -> Result<(), WireTxErrorKind> {
        if msg.len() > u32::MAX as usize {
            return Err(WireTxErrorKind::Other);
        }

        self.writer
            .lock()
            .await
            .write_all(&frame(&msg))
            .await
            .map_err(|_| WireTxErrorKind::Other)
    }
}

pub struct UnixStreamRx {
    reader: Mutex<OwnedReadHalf>,
}

impl UnixStreamRx {
    pub fn new(reader: OwnedReadHalf) -> Self {
        Self {
            reader: Mutex::new(reader),
        }
    }
}

impl PostcardReceiver for UnixStreamRx {
    async fn receive(&mut self) -> Result<Vec<u8>, WireRxErrorKind> {
        let mut reader = self.reader.lock().await;

        let mut length_buf = [0u8; LENGTH_PREFIX_LEN];
        match reader.read_exact(&mut length_buf).await {
            Ok(_) => {}
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Err(WireRxErrorKind::ConnectionClosed);
                }
                return Err(WireRxErrorKind::Other);
            }
        };

        let length = u32::from_le_bytes(length_buf) as usize;

        let mut msg = vec![0u8; length];
        reader
            .read_exact(&mut msg)
            .await
            .map_err(|_| WireRxErrorKind::Other)?;

        Ok(msg)
    }
}
