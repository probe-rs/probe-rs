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
use tokio_util::bytes::Bytes;

use crate::transport::{
    Deframer, frame,
    memory::{PostcardReceiver, PostcardSender},
};

// Receives length-prefixed binary messages from a websocket stream
pub struct WebsocketRx<S, E> {
    inner: S,
    deframer: Deframer,
    _marker: PhantomData<E>,
}

impl<S, E> WebsocketRx<S, E>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            deframer: Deframer::default(),
            _marker: PhantomData,
        }
    }

    async fn receive_inner(&mut self) -> Option<Result<Vec<u8>, E>> {
        loop {
            // Drain buffered messages first: a single websocket packet may carry
            // more than one, and the next packet may never arrive.
            if let Some(message) = self.deframer.next_message() {
                return Some(Ok(message));
            }

            match self.inner.next().await? {
                Ok(packet) => self.deframer.push(&packet),
                Err(e) => return Some(Err(e)),
            }
        }
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
    pub fn new(writer: S) -> Self {
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

        self.writer
            .lock()
            .await
            .send(Message::Binary(frame(&msg).freeze()))
            .await
            .map_err(|_| WireTxErrorKind::Other)
    }
}
