use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use postcard_rpc::host_client;
use postcard_rpc::{
    Topic,
    header::{VarHeader, VarKey, VarKeyKind, VarSeq},
    server::{self, WireRxErrorKind, WireTxErrorKind},
    standard_icd::LoggingTopic,
};
use serde::Serialize;
use tokio::sync::mpsc::{Receiver, Sender};

pub trait PostcardReceiver {
    fn receive(&mut self) -> impl Future<Output = Result<Vec<u8>, WireRxErrorKind>> + Send;
}

impl PostcardReceiver for Receiver<Result<Vec<u8>, WireRxErrorKind>> {
    async fn receive(&mut self) -> Result<Vec<u8>, WireRxErrorKind> {
        match self.recv().await {
            Some(packet) => packet,
            None => Err(WireRxErrorKind::ConnectionClosed),
        }
    }
}

impl PostcardReceiver for Receiver<Vec<u8>> {
    async fn receive(&mut self) -> Result<Vec<u8>, WireRxErrorKind> {
        match self.recv().await {
            Some(packet) => Ok(packet),
            None => Err(WireRxErrorKind::ConnectionClosed),
        }
    }
}

pub struct WireRx<R> {
    inner: R,
}

impl<R> WireRx<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: PostcardReceiver> server::WireRx for WireRx<R> {
    type Error = WireRxErrorKind;

    async fn receive<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a mut [u8], Self::Error> {
        let packet = self.inner.receive().await?;

        if packet.len() > buf.len() {
            return Err(WireRxErrorKind::ReceivedMessageTooLarge);
        }

        buf[..packet.len()].copy_from_slice(&packet);
        Ok(&mut buf[..packet.len()])
    }
}

#[derive(Debug, docsplay::Display, thiserror::Error)]
pub enum WireRxError {
    /// The connection has been closed.
    ConnectionClosed,
    /// The received message was too large for the server to handle
    ReceivedMessageTooLarge,
    /// Other message kinds
    Other,
}

impl<R: PostcardReceiver + Send + 'static> host_client::WireRx for WireRx<R> {
    type Error = WireRxError;

    async fn receive(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.inner.receive().await.map_err(|e| match e {
            WireRxErrorKind::ConnectionClosed => WireRxError::ConnectionClosed,
            WireRxErrorKind::ReceivedMessageTooLarge => WireRxError::ReceivedMessageTooLarge,
            _ => WireRxError::Other,
        })
    }
}

pub trait PostcardSender {
    fn send(&self, buf: Vec<u8>) -> impl Future<Output = Result<(), WireTxErrorKind>> + Send;
}

impl PostcardSender for Sender<Vec<u8>> {
    async fn send(&self, buf: Vec<u8>) -> Result<(), WireTxErrorKind> {
        Sender::send(self, buf)
            .await
            .map_err(|_| WireTxErrorKind::ConnectionClosed)
    }
}

impl PostcardSender for Sender<Result<Vec<u8>, WireRxErrorKind>> {
    async fn send(&self, buf: Vec<u8>) -> Result<(), WireTxErrorKind> {
        Sender::send(self, Ok(buf))
            .await
            .map_err(|_| WireTxErrorKind::ConnectionClosed)
    }
}

#[derive(Clone)]
pub struct WireTx<S: PostcardSender> {
    sink: S,
    log_seq: Arc<AtomicUsize>,
}

impl<S: PostcardSender> WireTx<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            log_seq: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl<S: PostcardSender> server::WireTx for WireTx<S> {
    type Error = WireTxErrorKind;

    async fn send<T: Serialize + ?Sized>(
        &self,
        hdr: postcard_rpc::header::VarHeader,
        msg: &T,
    ) -> Result<(), Self::Error> {
        // First, measure the length of the message
        let mut length_counter = LengthCounter(0);
        postcard::to_io(msg, &mut length_counter).unwrap();

        // Allocate a buffer for the message
        const HEADER_MAX_LEN: usize = 1 + 8;
        let mut buffer = Vec::with_capacity(length_counter.0 + HEADER_MAX_LEN);

        // Reserve space for the header
        buffer.extend(std::iter::repeat_n(0, HEADER_MAX_LEN));

        // Write the header
        let header_bytes = {
            let (used, _) = hdr.write_to_slice(&mut buffer[..HEADER_MAX_LEN]).unwrap();
            used.len()
        };

        // Trim back to the end of the header
        buffer.truncate(header_bytes);

        // Serialize the message
        let buffer = postcard::to_extend(msg, buffer).unwrap();

        // Send the message
        self.sink.send(buffer).await
    }

    async fn send_raw(&self, buf: &[u8]) -> Result<(), Self::Error> {
        self.sink.send(buf.to_vec()).await
    }

    async fn send_log_str(&self, kkind: VarKeyKind, s: &str) -> Result<(), Self::Error> {
        let key = match kkind {
            VarKeyKind::Key1 => VarKey::Key1(LoggingTopic::TOPIC_KEY1),
            VarKeyKind::Key2 => VarKey::Key2(LoggingTopic::TOPIC_KEY2),
            VarKeyKind::Key4 => VarKey::Key4(LoggingTopic::TOPIC_KEY4),
            VarKeyKind::Key8 => VarKey::Key8(LoggingTopic::TOPIC_KEY),
        };
        let ctr = self.log_seq.fetch_add(1, Ordering::Relaxed);
        let hdr = VarHeader {
            key,
            seq_no: VarSeq::Seq2((ctr & 0xFFFF) as u16),
        };

        self.send(hdr, s).await
    }

    async fn send_log_fmt(
        &self,
        kkind: VarKeyKind,
        a: std::fmt::Arguments<'_>,
    ) -> Result<(), Self::Error> {
        let s = format!("{a}");
        self.send_log_str(kkind, &s).await
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error, docsplay::Display)]
pub enum WireTxError {
    /// Transfer Error on Send: {0:?}
    Transfer(WireTxErrorKind),
}

impl From<WireTxErrorKind> for WireTxError {
    fn from(e: WireTxErrorKind) -> Self {
        WireTxError::Transfer(e)
    }
}

impl<S> host_client::WireTx for WireTx<S>
where
    S: PostcardSender + Send + Sync + 'static,
{
    type Error = WireTxError;

    async fn send(&mut self, buf: Vec<u8>) -> Result<(), Self::Error> {
        Ok(self.sink.send(buf).await?)
    }
}

struct LengthCounter(usize);
impl std::io::Write for LengthCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
