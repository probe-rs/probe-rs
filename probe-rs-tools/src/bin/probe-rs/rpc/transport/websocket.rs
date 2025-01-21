#[cfg(feature = "remote")]
use axum::extract::ws;
use futures_util::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use postcard_rpc::server::{WireRxErrorKind, WireTxErrorKind};
use std::{
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::bytes::{BufMut, Bytes, BytesMut};

use crate::rpc::transport::memory::{PostcardReceiver, PostcardSender};

// Receives length-prefixed binary messages from a websocket stream
pub struct WebsocketRx<S, E> {
    inner: S,
    buffer: Vec<u8>,
    _marker: PhantomData<E>,
}

impl<S, E> WebsocketRx<S, E>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            _marker: PhantomData,
        }
    }

    async fn receive_inner(&mut self) -> Option<Result<Vec<u8>, E>> {
        while let Some(packet) = self.inner.next().await {
            let packet = match packet {
                Ok(packet) => packet,
                Err(e) => return Some(Err(e)),
            };

            self.buffer.extend_from_slice(&packet);
            // Process length prefix encoding - try to read the length prefix
            if self.buffer.len() < 4 {
                continue;
            }

            let len = u32::from_le_bytes(self.buffer[0..4].try_into().unwrap()) as usize;

            if self.buffer.len() < len + 4 {
                continue;
            }

            let ret = self.buffer[4..][..len].to_vec();
            self.buffer.drain(..len + 4);

            return Some(Ok(ret));
        }

        None
    }
}

impl<S, E> Stream for WebsocketRx<S, E>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<Vec<u8>, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Safety: We don't move out of self
        std::pin::pin!(unsafe { self.get_unchecked_mut().receive_inner() }).poll_unpin(cx)
    }
}

impl<S, E> PostcardReceiver for WebsocketRx<S, E>
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin,
    E: Send,
{
    async fn receive(&mut self) -> Result<Vec<u8>, WireRxErrorKind> {
        match self.receive_inner().await {
            Some(Ok(packet)) => Ok(packet),
            Some(Err(_)) => Err(WireRxErrorKind::Other),
            None => Err(WireRxErrorKind::ConnectionClosed),
        }
    }
}

// Sends length-prefixed binary messages to a websocket stream
pub struct WebsocketTx<S> {
    writer: Arc<Mutex<S>>,
}
impl<S> WebsocketTx<S> {
    pub(crate) fn new(writer: S) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl<S> PostcardSender for WebsocketTx<S>
where
    S: Sink<Message> + Send + Sync + Unpin,
{
    async fn send(&self, msg: Vec<u8>) -> Result<(), WireTxErrorKind> {
        if msg.len() > u32::MAX as usize {
            return Err(WireTxErrorKind::Other);
        }

        let mut bytes = BytesMut::with_capacity(4 + msg.len());
        bytes.put_u32_le(msg.len() as u32);
        bytes.put_slice(&msg);

        self.writer
            .lock()
            .await
            .send(Message::Binary(bytes.freeze()))
            .await
            .map_err(|_| WireTxErrorKind::Other)
    }
}

// Sends length-prefixed binary messages to a websocket stream
pub struct AxumWebsocketTx<S> {
    writer: S,
}
impl<S> AxumWebsocketTx<S> {
    pub(crate) fn new(writer: S) -> Self {
        Self { writer }
    }
}

#[cfg(feature = "remote")]
impl<S> Sink<Vec<u8>> for AxumWebsocketTx<S>
where
    S: Sink<ws::Message> + Unpin,
{
    type Error = S::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, msg: Vec<u8>) -> Result<(), Self::Error> {
        let mut bytes = BytesMut::with_capacity(4 + msg.len());
        bytes.put_u32_le(msg.len() as u32);
        bytes.put_slice(&msg);

        self.writer
            .start_send_unpin(ws::Message::Binary(bytes.freeze()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.writer.poll_close_unpin(cx)
    }
}
