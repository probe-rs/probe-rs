use std::{
    fmt::{self, Debug, Formatter},
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    FutureExt,
    future::{self, BoxFuture},
};
use tower::Service;

use super::{
    ExitedError,
    client::{Client, ClientSocket},
    debug_adapter::dap::dap_types::{ErrorResponse, Request, Response},
    layers,
    protocol::response::ResponseKind,
    server::{
        DapServer,
        pending::Pending,
        router::{FromParams, IntoResponse, Method, Router},
    },
    state::{ServerState, State},
};

/// Service abstraction for the Language Server Protocol.
///
/// This service takes an incoming JSON-RPC message as input and produces an outgoing message as
/// output. If the incoming message is a server notification or a client response, then the
/// corresponding response will be `None`.
///
/// This implements [`tower::Service`] in order to remain independent from the underlying transport
/// and to facilitate further abstraction with middleware.
///
/// Pending requests can be canceled by issuing a [`$/cancelRequest`] notification.
///
/// [`$/cancelRequest`]: https://microsoft.github.io/language-server-protocol/specification#cancelRequest
///
/// The service shuts down and stops serving requests after the [`exit`] notification is received.
///
/// [`exit`]: https://microsoft.github.io/language-server-protocol/specification#exit
#[derive(Debug)]
pub struct DapService<S> {
    inner: Router<S, ExitedError>,
    state: Arc<ServerState>,
}

impl<S: DapServer> DapService<S> {
    /// Creates a new `DapService` with the given server backend, also returning a channel for
    /// server-to-client communication.
    pub fn new<F>(init: F) -> (Self, ClientSocket)
    where
        F: FnOnce(Client) -> S,
    {
        DapService::build(init).finish()
    }

    /// Starts building a new `DapService`.
    ///
    /// Returns an `DapServiceBuilder`, which allows adding custom JSON-RPC methods to the server.
    pub fn build<F>(init: F) -> DapServiceBuilder<S>
    where
        F: FnOnce(Client) -> S,
    {
        let state = Arc::new(ServerState::new());

        let (client, socket) = Client::new(state.clone());
        let inner = Router::new(init(client.clone()));
        let pending = Arc::new(Pending::new());

        DapServiceBuilder {
            inner: crate::generated::register_lsp_methods(
                inner,
                state.clone(),
                pending.clone(),
                client,
            ),
            state,
            pending,
            socket,
        }
    }

    /// Returns a reference to the inner server.
    pub fn inner(&self) -> &S {
        self.inner.inner()
    }
}

impl<S: DapServer> Service<Request> for DapService<S> {
    type Response = Option<ResponseKind>;
    type Error = ExitedError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.state.get() {
            State::Initializing => Poll::Pending,
            State::Exited => Poll::Ready(Err(ExitedError(()))),
            _ => self.inner.poll_ready(cx),
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if self.state.get() == State::Exited {
            return future::err(ExitedError(())).boxed();
        }

        let fut = self.inner.call(req);

        Box::pin(async move {
            let response = fut.await?;

            match response.as_ref().and_then(|res| res.error()) {
                Some(ErrorResponse {
                    message: Some(m), ..
                }) if m == "unknown method" => Ok(None),
                _ => Ok(response),
            }
        })
    }
}

/// A builder to customize the properties of an `DapService`.
///
/// To construct an `DapServiceBuilder`, refer to [`DapService::build`].
pub struct DapServiceBuilder<S> {
    inner: Router<S, ExitedError>,
    state: Arc<ServerState>,
    pending: Arc<Pending>,
    socket: ClientSocket,
}

impl<S: DapServer> DapServiceBuilder<S> {
    /// Defines a custom JSON-RPC request or notification with the given method `name` and handler.
    ///
    /// # Handler varieties
    ///
    /// Fundamentally, any inherent `async fn(&self)` method defined directly on the language
    /// server backend could be considered a valid method handler.
    ///
    /// Handlers may optionally include a single `params` argument. This argument may be of any
    /// type that implements [`Serialize`](serde::Serialize).
    ///
    /// Handlers which return `()` are treated as **notifications**, while those which return
    /// [`jsonrpc::Result<T>`](crate::jsonrpc::Result) are treated as **requests**.
    ///
    /// Similar to the `params` argument, the `T` in the `Result<T>` return values may be of any
    /// type which implements [`DeserializeOwned`](serde::de::DeserializeOwned). Additionally, this
    /// type _must_ be convertible into a [`serde_json::Value`] using [`serde_json::to_value`]. If
    /// this latter constraint is not met, the client will receive a JSON-RPC error response with
    /// code `-32603` (Internal Error) instead of the expected response.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use serde_json::{json, Value};
    /// use tower_lsp::jsonrpc::Result;
    /// use tower_lsp::lsp_types::*;
    /// use tower_lsp::{DapServer, DapService};
    ///
    /// struct Mock;
    ///
    /// // Implementation of `DapServer` omitted...
    /// # #[tower_lsp::async_trait]
    /// # impl DapServer for Mock {
    /// #     async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
    /// #         Ok(InitializeResult::default())
    /// #     }
    /// #
    /// #     async fn shutdown(&self) -> Result<()> {
    /// #         Ok(())
    /// #     }
    /// # }
    ///
    /// impl Mock {
    ///     async fn request(&self) -> Result<i32> {
    ///         Ok(123)
    ///     }
    ///
    ///     async fn request_params(&self, params: Vec<String>) -> Result<Value> {
    ///         Ok(json!({"num_elems":params.len()}))
    ///     }
    ///
    ///     async fn notification(&self) {
    ///         // ...
    ///     }
    ///
    ///     async fn notification_params(&self, params: Value) {
    ///         // ...
    /// #       let _ = params;
    ///     }
    /// }
    ///
    /// let (service, socket) = DapService::build(|_| Mock)
    ///     .custom_method("custom/request", Mock::request)
    ///     .custom_method("custom/requestParams", Mock::request_params)
    ///     .custom_method("custom/notification", Mock::notification)
    ///     .custom_method("custom/notificationParams", Mock::notification_params)
    ///     .finish();
    /// ```
    pub fn custom_method<P, R, F>(mut self, name: &'static str, callback: F) -> Self
    where
        P: FromParams,
        R: IntoResponse,
        F: for<'a> Method<&'a S, P, R> + Clone + Send + Sync + 'static,
    {
        let layer = layers::Normal::new(self.state.clone(), self.pending.clone());
        self.inner.method(name, callback, layer);
        self
    }

    /// Constructs the `DapService` and returns it, along with a channel for server-to-client
    /// communication.
    pub fn finish(self) -> (DapService<S>, ClientSocket) {
        let DapServiceBuilder {
            inner,
            state,
            socket,
            ..
        } = self;

        (DapService { inner, state }, socket)
    }
}

impl<S: Debug> Debug for DapServiceBuilder<S> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("DapServiceBuilder")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}
