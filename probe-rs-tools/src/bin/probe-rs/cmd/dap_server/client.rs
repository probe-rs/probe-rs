//! Types for sending data to and from the language client.

mod pending;
// pub mod progress;
mod socket;

pub use self::socket::{ClientSocket, RequestStream, ResponseSink};

use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context, Poll};

use futures::channel::mpsc::{self, Sender};
use futures::future::BoxFuture;
use futures::sink::SinkExt;
use serde_json::Value;
use tower::Service;
use tracing::{error, trace};

use self::pending::Pending;
// use self::progress::Progress;

use super::debug_adapter::codec::Message;
use super::debug_adapter::dap::dap_types::{
    Breakpoint, BreakpointEvent, BreakpointEventBody, RunInTerminalRequest,
    RunInTerminalRequestArguments, RunInTerminalResponseBody, StartDebuggingRequest,
    StartDebuggingRequestArguments,
};
use super::protocol::response::ResponseKind;
use super::protocol::{EventData, RequestData, Sendable};
use super::state::{ServerState, State};
use super::{DebuggerError, ExitedError};

struct ClientInner {
    tx: Sender<Message>,
    request_id: AtomicI64,
    pending: Arc<Pending>,
    state: Arc<ServerState>,
}

/// Handle for communicating with the language client.
///
/// This type provides a very cheap implementation of [`Clone`] so API consumers can cheaply clone
/// and pass it around as needed.
///
/// It also implements [`tower::Service`] in order to remain independent from the underlying
/// transport and to facilitate further abstraction with middleware.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

impl Client {
    pub(super) fn new(state: Arc<ServerState>) -> (Self, ClientSocket) {
        let (tx, rx) = mpsc::channel(1);
        let pending = Arc::new(Pending::new());

        let client = Client {
            inner: Arc::new(ClientInner {
                tx,
                request_id: AtomicI64::new(0),
                pending: pending.clone(),
                state: state.clone(),
            }),
        };

        (client, ClientSocket { rx, pending, state })
    }

    /// Disconnects the `Client` from its corresponding `LspService`.
    ///
    /// Closing the client is not required, but doing so will ensure that no more messages can be
    /// produced. The receiver of the messages will be able to consume any in-flight messages and
    /// then will observe the end of the stream.
    ///
    /// If the client is never closed and never dropped, the receiver of the messages will never
    /// observe the end of the stream.
    pub(crate) fn close(&self) {
        self.inner.tx.clone().close_channel();
    }
}

impl Client {
    // Lifecycle Messages

    /// Registers a new capability with the client.
    ///
    /// This corresponds to the [`client/registerCapability`] request.
    ///
    /// [`client/registerCapability`]: https://microsoft.github.io/language-server-protocol/specification#client_registerCapability
    ///
    /// # Initialization
    ///
    /// If the request is sent to the client before the server has been initialized, this will
    /// immediately return `Err` with JSON-RPC error code `-32002` ([read more]).
    ///
    /// [read more]: https://microsoft.github.io/language-server-protocol/specification#initialize
    pub async fn run_in_terminal(
        &self,
        arguments: RunInTerminalRequestArguments,
    ) -> Result<RunInTerminalResponseBody, DebuggerError> {
        self.send_request::<RunInTerminalRequest>(arguments).await
    }

    /// Unregisters a capability with the client.
    ///
    /// This corresponds to the [`client/unregisterCapability`] request.
    ///
    /// [`client/unregisterCapability`]: https://microsoft.github.io/language-server-protocol/specification#client_unregisterCapability
    ///
    /// # Initialization
    ///
    /// If the request is sent to the client before the server has been initialized, this will
    /// immediately return `Err` with JSON-RPC error code `-32002` ([read more]).
    ///
    /// [read more]: https://microsoft.github.io/language-server-protocol/specification#initialize
    pub async fn start_debugging(
        &self,
        arguments: StartDebuggingRequestArguments,
    ) -> Result<Option<Value>, DebuggerError> {
        self.send_request::<StartDebuggingRequest>(arguments).await
    }

    /// Notifies the client to log a particular message.
    ///
    /// This corresponds to the [`window/logMessage`] notification.
    ///
    /// [`window/logMessage`]: https://microsoft.github.io/language-server-protocol/specification#window_logMessage
    pub async fn breakpoint(&self, reason: String, breakpoint: Breakpoint) {
        self.send_event_unchecked::<BreakpointEvent>(BreakpointEventBody { reason, breakpoint })
            .await;
    }

    // /// Starts a stream of `$/progress` events for a client-provided [`ProgressToken`].
    // ///
    // /// This method also takes a `title` argument briefly describing the kind of operation being
    // /// performed, e.g. "Indexing" or "Linking Dependencies".
    // ///
    // /// [`ProgressToken`]: https://docs.rs/lsp-types/latest/lsp_types/type.ProgressToken.html
    // ///
    // /// # Initialization
    // ///
    // /// These events will only be sent if the server is initialized.
    // ///
    // /// # Examples
    // ///
    // /// ```no_run
    // /// # use tower_lsp::{lsp_types::*, Client};
    // /// #
    // /// # struct Mock {
    // /// #     client: Client,
    // /// # }
    // /// #
    // /// # impl Mock {
    // /// # async fn completion(&self, params: CompletionParams) {
    // /// # let work_done_token = ProgressToken::Number(1);
    // /// #
    // /// let progress = self
    // ///     .client
    // ///     .progress(work_done_token, "Progress Title")
    // ///     .with_message("Working...")
    // ///     .with_percentage(0)
    // ///     .begin()
    // ///     .await;
    // ///
    // /// for percent in 1..=100 {
    // ///     let msg = format!("Working... [{percent}/100]");
    // ///     progress.report_with_message(msg, percent).await;
    // /// }
    // ///
    // /// progress.finish_with_message("Done!").await;
    // /// # }
    // /// # }
    // /// ```
    // pub fn progress<T>(&self, token: ProgressToken, title: T) -> Progress
    // where
    //     T: Into<String>,
    // {
    //     Progress::new(self.clone(), token, title.into())
    // }

    /// Sends a custom event to the client.
    ///
    /// # Initialization
    ///
    /// This event will only be sent if the server is initialized.
    pub async fn send_event<E>(&self, params: E::Body)
    where
        E: EventData,
    {
        if let State::Initialized | State::ShutDown = self.inner.state.get() {
            self.send_event_unchecked::<E>(params).await;
        } else {
            let msg = E::new(params);
            trace!("server not initialized, supressing message: {:?}", msg);
        }
    }

    async fn send_event_unchecked<E>(&self, params: E::Body)
    where
        E: EventData,
    {
        let request = E::new(params);
        if self.clone().call(request).await.is_err() {
            error!("failed to send event");
        }
    }

    /// Sends a custom request to the client.
    ///
    /// # Initialization
    ///
    /// If the request is sent to the client before the server has been initialized, this will
    /// immediately return `Err` with JSON-RPC error code `-32002` ([read more]).
    ///
    /// [read more]: https://microsoft.github.io/language-server-protocol/specification#initialize
    pub async fn send_request<R>(&self, params: R::Args) -> Result<R::Result, DebuggerError>
    where
        R: RequestData,
    {
        if let State::Initialized | State::ShutDown = self.inner.state.get() {
            self.send_request_unchecked::<R>(params).await
        } else {
            let id = self.inner.request_id.load(Ordering::SeqCst) as i64 + 1;
            let msg = R::new(id.into(), params);
            tracing::trace!("server not initialized, supressing message: {:?}", msg);
            Err(DebuggerError::UninitializedServer)
        }
    }

    async fn send_request_unchecked<R>(&self, params: R::Args) -> Result<R::Result, DebuggerError>
    where
        R: RequestData,
    {
        let seq = self.next_seq();
        let request = R::new(seq, params);

        let response = match self.clone().call(request).await {
            Ok(Some(response)) => response,
            Ok(None) | Err(_) => return Err(DebuggerError::Internal),
        };

        let (_, result) = response.into_parts();
        result.map_err(DebuggerError::ErrorResponse).and_then(|v| {
            serde_json::from_value(v.body.unwrap_or_default()).map_err(DebuggerError::JsonParse)
        })
    }
}

impl Client {
    /// Increments the internal request ID counter and returns the previous value.
    ///
    /// This method can be used to build custom [`Request`] objects with numeric IDs that are
    /// guaranteed to be unique every time.
    pub fn next_seq(&self) -> i64 {
        self.inner.request_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl Debug for Client {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("Client")
            .field("tx", &self.inner.tx)
            .field("pending", &self.inner.pending)
            .field("request_id", &self.inner.request_id)
            .field("state", &self.inner.state)
            .finish()
    }
}

impl<S: Sendable> Service<S> for Client {
    type Response = Option<ResponseKind>;
    type Error = ExitedError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner
            .tx
            .clone()
            .poll_ready(cx)
            .map_err(|_| ExitedError(()))
    }

    fn call(&mut self, req: S) -> Self::Future {
        let mut tx = self.inner.tx.clone();
        let response_waiter = req.seq().map(|id| self.inner.pending.wait(id));

        Box::pin(async move {
            if tx.send(req.to_message()).await.is_err() {
                return Err(ExitedError(()));
            }

            match response_waiter {
                Some(fut) => Ok(Some(fut.await)),
                None => Ok(None),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use futures::stream::StreamExt;

    use crate::cmd::dap_server::debug_adapter::dap::dap_types::{
        Breakpoint, BreakpointEvent, BreakpointEventBody,
    };

    use super::*;

    async fn assert_client_message<F, Fut>(f: F, expected: Message)
    where
        F: FnOnce(Client) -> Fut,
        Fut: Future,
    {
        let state = Arc::new(ServerState::new());
        state.set(State::Initialized);

        let (client, socket) = Client::new(state);
        f(client).await;

        let messages: Vec<_> = socket.collect().await;
        assert_eq!(messages, vec![expected]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn breakpoint() {
        let (reason, breakpoint) = (
            "no reason".to_string(),
            Breakpoint {
                column: None,
                end_column: None,
                end_line: None,
                id: None,
                instruction_reference: None,
                line: None,
                message: None,
                offset: None,
                source: None,
                verified: false,
            },
        );
        let expected = BreakpointEvent::new(BreakpointEventBody {
            breakpoint: breakpoint.clone(),
            reason: reason.clone(),
        });

        assert_client_message(
            |p| async move { p.breakpoint(reason, breakpoint).await },
            expected.to_message(),
        )
        .await;
    }
}
