use super::{
    backends::socket::{ClientSocket, RequestStream, ResponseSink},
    debugger::Debugger,
};
use crate::cmd::dap_server::debug_adapter::{
    codec::{decoder::MsDapDecoder, message::Message},
    dap::{
        adapter::*,
        dap_types::{Request, Response},
    },
    protocol::DapAdapter,
};
use anyhow::{Context, Result};
use futures_util::{AsyncRead, AsyncWrite, Sink, SinkExt, Stream, TryFutureExt, future, stream};
use probe_rs::probe::list::Lister;
use serde::Deserialize;
use std::{
    fs,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    sync::mpsc::{self, unbounded_channel},
};
use tokio_util::{
    bytes::BytesMut,
    codec::{Decoder, FramedRead, FramedWrite},
};
use tower::Service;

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
pub(crate) enum TargetSessionType {
    AttachRequest,
    LaunchRequest,
}

impl std::str::FromStr for TargetSessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "attach" => Ok(TargetSessionType::AttachRequest),
            "launch" => Ok(TargetSessionType::LaunchRequest),
            _ => Err(format!(
                "'{s}' is not a valid target session type. Can be either 'attach' or 'launch']."
            )),
        }
    }
}

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const MESSAGE_QUEUE_SIZE: usize = 100;

/// Trait implemented by client loopback sockets.
///
/// This socket handles the server-to-client half of the bidirectional communication stream.
pub trait Loopback {
    /// Yields a stream of pending server-to-client requests.
    type RequestStream: Stream<Item = Request>;
    /// Routes client-to-server responses back to the server.
    type ResponseSink: Sink<Response> + Unpin;

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
    <L::ResponseSink as Sink<Response>>::Error: std::error::Error,
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
        T: Service<Request, Response = Option<Response>> + Send + 'static,
        T::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        T::Future: Send,
    {
        let (client_requests, mut client_responses) = self.loopback.split();
        let (client_requests, client_abort) = stream::abortable(client_requests);
        let (mut responses_tx, responses_rx) = mpsc::channel(0);
        let (mut server_tasks_tx, server_tasks_rx) = mpsc::channel(MESSAGE_QUEUE_SIZE);

        let mut framed_stdin = FramedRead::new(self.stdin, MsDapDecoder::default());
        let framed_stdout = FramedWrite::new(self.stdout, LanguageServerCodec::default());

        let process_server_tasks = server_tasks_rx
            .buffer_unordered(self.max_concurrency)
            .filter_map(future::ready)
            .map(|res| Ok(Message::Response(res)))
            .forward(responses_tx.clone().sink_map_err(|_| unreachable!()))
            .map(|_| ());

        let produce_output = stream::select(responses_rx, client_requests.map(Message::Request))
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
                    Err(err) => {
                        tracing::error!("failed to decode message: {}", err);
                        let res = Response::from_error(Id::Null, to_jsonrpc_error(err));
                        responses_tx.send(Message::Response(res)).await.unwrap();
                    }
                }
            }

            server_tasks_tx.disconnect();
            responses_tx.disconnect();
            client_abort.abort();
        };

        tokio::join!(produce_output, read_input, process_server_tasks);
    }
}

/// Display a whole error source trace for any given error.
fn display_sources(error: &dyn std::error::Error) -> String {
    if let Some(source) = error.source() {
        format!("{}: {}", error, display_sources(source))
    } else {
        error.to_string()
    }
}

pub async fn debug(
    lister: &Lister,
    addr: std::net::SocketAddr,
    single_session: bool,
    log_file: Option<&Path>,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset, log_file)?;

    let old_hook = std::panic::take_hook();
    let logger = debugger.debug_logger.clone();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Flush logs before printing panic.
        _ = logger.flush();
        old_hook(panic_info);
    }));

    loop {
        let listener = TcpListener::bind(addr).await?;

        debugger
            .debug_logger
            .log_to_console(&format!("Listening for requests on port {}", addr.port()))?;

        if !single_session {
            // When running as a server from the command line, we want startup logs to go to the stderr.
            debugger.debug_logger.flush()?;
        }

        match listener.accept().await {
            Ok((socket, addr)) => {
                debugger
                    .debug_logger
                    .log_to_console(&format!("Starting debug session from: {addr}"))?;

                let (reader, writer) = socket.into_split();
                let writer = socket;

                let (sender, receiver) = unbounded_channel();

                let mut decoder = MsDapDecoder::new();
                let dap_adapter = DapAdapter::new(reader, writer);
                let mut debug_adapter = DebugAdapter::new(dap_adapter);

                let mut buf = BytesMut::new();

                loop {
                    reader.read_buf(&mut buf).await?;

                    while let Some(frame) = decoder.decode(&mut buf)? {
                        sender.send(frame);
                    }

                    // Flush any pending log messages to the debug adapter Console Log.
                    debugger.debug_logger.flush_to_dap(&mut debug_adapter)?;

                    let end_message = match debugger.debug_session(debug_adapter, lister).await {
                        // We no longer have a reference to the `debug_adapter`, so errors need
                        // special handling to ensure they are displayed to the user.
                        Err(error) => format!("Session ended: {error}"),
                        Ok(()) => format!("Closing debug session from: {addr}"),
                    };
                    debugger.debug_logger.log_to_console(&end_message)?;

                    // Terminate after a single debug session. This is the behavour expected by VSCode
                    // if it started probe-rs as a child process.
                    if single_session {
                        break;
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    "probe-rs-debugger failed to establish a socket connection. Reason: {:?}",
                    error
                );
            }
        }
        debugger.debug_logger.flush()?;
    }

    debugger
        .debug_logger
        .log_to_console("DAP Protocol server exiting")?;

    debugger.debug_logger.flush()?;

    Ok(())
}

/// Try to get the timestamp of a file.
///
/// If an error occurs, None is returned.
pub(crate) fn get_file_timestamp(path_to_elf: &Path) -> Option<Duration> {
    fs::metadata(path_to_elf)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
}
