//! Loopback connection to the language client.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::channel::mpsc::Receiver;
use futures::sink::Sink;
use futures::stream::{FusedStream, Stream, StreamExt};

use crate::cmd::dap_server::ExitedError;
use crate::cmd::dap_server::protocol::response::ResponseKind;
use crate::cmd::dap_server::state::{ServerState, State};

use super::{Message, Pending};

/// A loopback channel for server-to-client communication.
#[derive(Debug)]
pub struct ClientSocket {
    pub(super) rx: Receiver<Message>,
    pub(super) pending: Arc<Pending>,
    pub(super) state: Arc<ServerState>,
}

impl ClientSocket {
    /// Splits this `ClientSocket` into two halves capable of operating independently.
    ///
    /// The two halves returned implement the [`Stream`] and [`Sink`] traits, respectively.
    ///
    /// [`Stream`]: futures::Stream
    /// [`Sink`]: futures::Sink
    pub fn split(self) -> (RequestStream, ResponseSink) {
        let ClientSocket { rx, pending, state } = self;
        let state_ = state.clone();

        (
            RequestStream { rx, state: state_ },
            ResponseSink { pending, state },
        )
    }
}

/// Yields a stream of pending server-to-client requests.
impl Stream for ClientSocket {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.state.get() == State::Exited || self.rx.is_terminated() {
            Poll::Ready(None)
        } else {
            self.rx.poll_next_unpin(cx)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rx.size_hint()
    }
}

impl FusedStream for ClientSocket {
    fn is_terminated(&self) -> bool {
        self.rx.is_terminated()
    }
}

/// Routes client-to-server responses back to the server.
impl Sink<ResponseKind> for ClientSocket {
    type Error = ExitedError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.state.get() == State::Exited || self.rx.is_terminated() {
            Poll::Ready(Err(ExitedError(())))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: ResponseKind) -> Result<(), Self::Error> {
        self.pending.insert(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Yields a stream of pending server-to-client requests.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct RequestStream {
    rx: Receiver<Message>,
    state: Arc<ServerState>,
}

impl Stream for RequestStream {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.state.get() == State::Exited || self.rx.is_terminated() {
            Poll::Ready(None)
        } else {
            self.rx.poll_next_unpin(cx)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rx.size_hint()
    }
}

impl FusedStream for RequestStream {
    fn is_terminated(&self) -> bool {
        self.rx.is_terminated()
    }
}

/// Routes client-to-server responses back to the server.
#[derive(Debug)]
pub struct ResponseSink {
    pending: Arc<Pending>,
    state: Arc<ServerState>,
}

impl Sink<ResponseKind> for ResponseSink {
    type Error = ExitedError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.state.get() == State::Exited {
            Poll::Ready(Err(ExitedError(())))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn start_send(self: Pin<&mut Self>, item: ResponseKind) -> Result<(), Self::Error> {
        self.pending.insert(item);
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}
