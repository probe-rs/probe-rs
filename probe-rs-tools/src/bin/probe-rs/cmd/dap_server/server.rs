/// All the shared options that control the behaviour of the debugger.
pub(crate) mod configuration;
/// The data structures borrowed from the [`session_data::SessionData`], that applies to a specific core.
pub(crate) mod core_data;
/// The debugger support for rtt.
pub(crate) mod debug_rtt;
/// Implements the part of the debug server that processes incoming requests from the [`DebugAdapter`](crate::cmd::dap_server::debug_adapter::dap::adapter::DebugAdapter).
pub(crate) mod debugger;
/// Manage the logging/tracing associated with the debugger.
pub(crate) mod logger;
pub(crate) mod pending;
pub(crate) mod router;
/// The data structures needed to keep track of a session status in the debugger.
pub(crate) mod session_data;
/// This is where the primary processing for the debugger is driven from.
pub(crate) mod startup;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite};

use futures::channel::mpsc;
use futures::{FutureExt, Sink, SinkExt, Stream, StreamExt, TryFutureExt, future, join, stream};
use tower::Service;

use crate::cmd::dap_server::debug_adapter::codec::{DapCodec, Message};
use crate::cmd::dap_server::debug_adapter::dap::dap_types::{ErrorResponse, ErrorResponseBody};

use super::client::{ClientSocket, RequestStream, ResponseSink};
use super::debug_adapter::dap::dap_types::Request;
use super::protocol::response::ResponseKind;

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const MESSAGE_QUEUE_SIZE: usize = 100;

#[async_trait::async_trait]
#[auto_impl::auto_impl(Arc, Box)]
pub trait DapServer: Send + Sync + 'static {}

pub struct Xyz {}

impl DapServer for Xyz {}

/// Trait implemented by client loopback sockets.
///
/// This socket handles the server-to-client half of the bidirectional communication stream.
pub trait Loopback {
    /// Yields a stream of pending server-to-client requests.
    type RequestStream: Stream<Item = Message>;
    /// Routes client-to-server responses back to the server.
    type ResponseSink: Sink<ResponseKind> + Unpin;

    /// Splits this socket into two halves capable of operating independently.
    ///
    /// The two halves returned implement the [`Stream`] and [`Sink`] traits, respectively.
    fn split(self) -> (Self::RequestStream, Self::ResponseSink);
}

impl Loopback for ClientSocket {
    type RequestStream = RequestStream;
    type ResponseSink = ResponseSink;

    #[inline]
    fn split(self) -> (Self::RequestStream, Self::ResponseSink) {
        self.split()
    }
}

/// Server for processing requests and responses on standard I/O or TCP.
#[derive(Debug)]
pub struct Server<I, O, L = ClientSocket> {
    stdin: I,
    stdout: O,
    loopback: L,
    max_concurrency: usize,
}

impl<I, O, L> Server<I, O, L>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite,
    L: Loopback,
    <L::ResponseSink as Sink<ResponseKind>>::Error: std::error::Error,
{
    /// Creates a new `Server` with the given `stdin` and `stdout` handles.
    pub fn new(stdin: I, stdout: O, socket: L) -> Self {
        Server {
            stdin,
            stdout,
            loopback: socket,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
        }
    }

    /// Sets the server concurrency limit to `max`.
    ///
    /// This setting specifies how many incoming requests may be processed concurrently. Setting
    /// this value to `1` forces all requests to be processed sequentially, thereby implicitly
    /// disabling support for the [`$/cancelRequest`] notification.
    ///
    /// [`$/cancelRequest`]: https://microsoft.github.io/language-server-protocol/specification#cancelRequest
    ///
    /// If not explicitly specified, `max` defaults to 4.
    ///
    /// # Preference over standard `tower` middleware
    ///
    /// The [`ConcurrencyLimit`] and [`Buffer`] middlewares provided by `tower` rely on
    /// [`tokio::spawn`] in common usage, while this library aims to be executor agnostic and to
    /// support exotic targets currently incompatible with `tokio`, such as WASM. As such, `Server`
    /// includes its own concurrency facilities that don't require a global executor to be present.
    ///
    /// [`ConcurrencyLimit`]: https://docs.rs/tower/latest/tower/limit/concurrency/struct.ConcurrencyLimit.html
    /// [`Buffer`]: https://docs.rs/tower/latest/tower/buffer/index.html
    /// [`tokio::spawn`]: https://docs.rs/tokio/latest/tokio/fn.spawn.html
    pub fn concurrency_level(mut self, max: usize) -> Self {
        self.max_concurrency = max;
        self
    }

    /// Spawns the service with messages read through `stdin` and responses written to `stdout`.
    pub async fn serve<T>(self, mut service: T)
    where
        T: Service<Request, Response = Option<ResponseKind>> + Send + 'static,
        T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        T::Future: Send,
    {
        let (client_requests, mut client_responses) = self.loopback.split();
        let (client_requests, client_abort) = stream::abortable(client_requests);
        let (mut responses_tx, responses_rx) = mpsc::channel(0);
        let (mut server_tasks_tx, server_tasks_rx) = mpsc::channel(MESSAGE_QUEUE_SIZE);

        let mut framed_stdin = FramedRead::new(self.stdin, DapCodec::default());
        let framed_stdout = FramedWrite::new(self.stdout, DapCodec::default());

        let process_server_tasks = server_tasks_rx
            .buffer_unordered(self.max_concurrency)
            .filter_map(future::ready)
            .map(|res| Ok(Message::Response(res)))
            .forward(responses_tx.clone().sink_map_err(|_| unreachable!()))
            .map(|_| ());

        let print_output = stream::select(responses_rx, client_requests)
            .map(Ok)
            .forward(
                framed_stdout.sink_map_err(|e| tracing::error!("failed to encode message: {}", e)),
            )
            .map(|_| ());

        let read_input = async {
            while let Some(msg) = framed_stdin.next().await {
                match msg {
                    Ok(Message::Request(req)) => {
                        if let Err(err) = future::poll_fn(|cx| service.poll_ready(cx)).await {
                            tracing::error!("{}", display_sources(err.into().as_ref()));
                            return;
                        }

                        let fut = service.call(req).unwrap_or_else(|err| {
                            tracing::error!("{}", display_sources(err.into().as_ref()));
                            None
                        });

                        server_tasks_tx.send(fut).await.unwrap();
                    }
                    Ok(Message::Response(res)) => {
                        if let Err(err) = client_responses.send(res).await {
                            tracing::error!("{}", display_sources(&err));
                            return;
                        }
                    }
                    Ok(Message::Event(event)) => {
                        tracing::error!("events should not land here: {:?}", event);
                        continue;
                    }
                    Err(err) => {
                        tracing::error!("failed to decode message: {}", err);
                        let res = ResponseKind::error_from_nothing(Some(format!("{err:?}")), None);
                        responses_tx.send(Message::Response(res)).await.unwrap();
                        continue;
                    }
                }
            }

            server_tasks_tx.disconnect();
            responses_tx.disconnect();
            client_abort.abort();
        };

        join!(print_output, read_input, process_server_tasks);
    }
}

fn display_sources(error: &dyn std::error::Error) -> String {
    if let Some(source) = error.source() {
        format!("{}: {}", error, display_sources(source))
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll};

    use std::io::Cursor;

    use futures::future::Ready;
    use futures::{future, sink, stream};

    use super::*;

    const REQUEST: &str = r#"{"command":"initialize","seq":1,"type":"request"}"#;
    const RESPONSE: &str = r#"{"command":"initialize","message":"cancelled","request_seq":1,"seq":1,"success":true,"type":"response"}"#;

    #[derive(Debug)]
    struct MockService;

    impl Service<Request> for MockService {
        type Response = Option<ResponseKind>;
        type Error = String;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request) -> Self::Future {
            let response = serde_json::from_str(RESPONSE).unwrap();
            future::ok(Some(response))
        }
    }

    struct MockLoopback(Vec<Message>);

    impl Loopback for MockLoopback {
        type RequestStream = stream::Iter<std::vec::IntoIter<Message>>;
        type ResponseSink = sink::Drain<ResponseKind>;

        fn split(self) -> (Self::RequestStream, Self::ResponseSink) {
            (stream::iter(self.0), sink::drain())
        }
    }

    fn mock_request() -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{}", REQUEST.len(), REQUEST).into_bytes()
    }

    fn mock_response() -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{}", RESPONSE.len(), RESPONSE).into_bytes()
    }

    fn mock_stdio() -> (Cursor<Vec<u8>>, Vec<u8>) {
        (Cursor::new(mock_request()), Vec::new())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serves_on_stdio() {
        let (mut stdin, mut stdout) = mock_stdio();
        Server::new(&mut stdin, &mut stdout, MockLoopback(vec![]))
            .serve(MockService)
            .await;

        assert_eq!(stdin.position(), 71);
        assert_eq!(stdout, mock_response());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interleaves_messages() {
        let socket = MockLoopback(vec![Message::Request(Request {
            arguments: None,
            command: "initialize".into(),
            seq: 1,
            type_: "request".into(),
        })]);

        let (mut stdin, mut stdout) = mock_stdio();
        Server::new(&mut stdin, &mut stdout, socket)
            .serve(MockService)
            .await;

        assert_eq!(stdin.position(), 71);
        let output: Vec<_> = mock_request().into_iter().chain(mock_response()).collect();
        pretty_assertions::assert_eq!(
            String::from_utf8_lossy(&stdout).to_string(),
            String::from_utf8_lossy(&output).to_string()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handles_invalid_json() {
        let invalid = r#"{"jsonrpc":"2.0","method":"#;
        let message = format!("Content-Length: {}\r\n\r\n{}", invalid.len(), invalid).into_bytes();
        let (mut stdin, mut stdout) = (Cursor::new(message), Vec::new());

        Server::new(&mut stdin, &mut stdout, MockLoopback(vec![]))
            .serve(MockService)
            .await;

        assert_eq!(stdin.position(), 48);
        let err = r#"{"body":{},"command":"","message":"Custom { kind: UnexpectedEof, error: Error(\"EOF while parsing a value\", line: 1, column: 26) }","request_seq":0,"seq":0,"success":false,"type":"response"}"#;
        let output = format!("Content-Length: {}\r\n\r\n{}", err.len(), err).into_bytes();
        pretty_assertions::assert_eq!(
            String::from_utf8_lossy(&stdout).to_string(),
            String::from_utf8_lossy(&output).to_string()
        );
    }
}
