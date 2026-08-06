//! A websocket transport that runs over `ssh -W`.
//!
//! ssh forwards its stdio to `127.0.0.1:<port>` on the remote host, so the
//! server must listen on the loopback interface there. The client gives ssh
//! only the destination: every other setting, for example the identity file,
//! a jump host, or an ssh port other than 22, comes from the ssh
//! configuration file of the user.

use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use http::Uri;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader, ReadBuf};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::oneshot;
use tokio_tungstenite::{client_async, tungstenite::ClientRequestBuilder};

use crate::{ClientError, RpcClient, TransportError};

const DEFAULT_PORT: u16 = 3000;
const STDERR_TAIL_LINES: usize = 10;

type StderrTail = Arc<Mutex<Vec<String>>>;

/// Parse `[user@]destination[:port]` from an `ssh://` URL (without the prefix).
fn parse_ssh_connect(host: &str) -> Result<(String, u16), ClientError> {
    if host.is_empty() {
        return Err(ClientError::InvalidRemoteHost);
    }

    let Some(colon) = host.rfind(':') else {
        return Ok((unbracket(host), DEFAULT_PORT));
    };

    let (destination, port) = host.split_at(colon);
    let port = &port[1..];

    // A bare IPv6 literal such as `fe80::1` holds colons of its own. Only the
    // bracketed form can carry a port.
    if destination.contains(':') && !destination.ends_with(']') {
        return Ok((unbracket(host), DEFAULT_PORT));
    }

    let destination = unbracket(destination);
    if destination.is_empty() {
        return Err(ClientError::InvalidRemoteHost);
    }

    let port = port
        .parse::<u16>()
        .map_err(|_| ClientError::InvalidRemoteHost)?;

    Ok((destination, port))
}

/// Remove the brackets around an IPv6 literal. ssh takes the destination
/// without them, in both the `host` and the `user@host` form.
fn unbracket(destination: &str) -> String {
    let (user, host) = match destination.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, destination),
    };

    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);

    match user {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    }
}

pub async fn connect(
    host: &str,
    token: Option<&str>,
    user_agent: &str,
) -> Result<RpcClient, ClientError> {
    let (destination, port) = parse_ssh_connect(host)?;

    let mut child = Command::new("ssh")
        .arg(&destination)
        .arg("-W")
        .arg(format!("127.0.0.1:{port}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(ClientError::SshSpawn)?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stdin = child.stdin.take().expect("stdin is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let tail = StderrTail::default();
    tokio::spawn(relay_stderr(stderr, tail.clone()));

    let (alive, mut exited) = supervise(child);
    let stream = tokio::io::join(
        GuardedRead {
            inner: stdout,
            _alive: alive,
        },
        stdin,
    );

    let uri = Uri::from_str(&format!("ws://127.0.0.1:{port}/worker"))
        .map_err(|_| ClientError::InvalidRemoteHost)?;
    let req = ClientRequestBuilder::new(uri).with_header("User-Agent", user_agent);

    let (ws_stream, resp) = tokio::select! {
        result = client_async(req, stream) => {
            result.map_err(|err| with_tail(format!("Could not connect: {err}"), &tail))?
        }
        status = &mut exited => {
            return Err(with_tail(
                format!("Could not connect: ssh {}", describe(status)),
                &tail,
            )
            .into());
        }
    };

    let challenge = resp
        .headers()
        .get("Probe-Rs-Challenge")
        .ok_or(TransportError::Message("No challenge header".into()))?
        .to_str()
        .map_err(|_| TransportError::Message("Failed to parse challenge header".into()))?;

    let client = crate::rpc_client_from_websocket(ws_stream, challenge, token).await?;

    tokio::spawn(report_exit(exited, tail));

    Ok(client)
}

/// Wait for ssh in a task that owns the child, because [`Child::wait`] needs
/// exclusive access for as long as the session runs.
///
/// The returned sender keeps ssh alive: dropping it kills the child. The
/// receiver reports an exit that probe-rs did not ask for, and stays silent
/// when the sender was dropped.
fn supervise(mut child: Child) -> (oneshot::Sender<()>, oneshot::Receiver<ExitStatus>) {
    let (alive, mut dropped) = oneshot::channel::<()>();
    let (exit_tx, exit_rx) = oneshot::channel();

    tokio::spawn(async move {
        tokio::select! {
            status = child.wait() => {
                if let Ok(status) = status {
                    let _ = exit_tx.send(status);
                }
            }
            _ = &mut dropped => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    });

    (alive, exit_rx)
}

async fn report_exit(exited: oneshot::Receiver<ExitStatus>, tail: StderrTail) {
    let Ok(status) = exited.await else {
        return;
    };

    if status.success() {
        tracing::debug!("ssh {}", describe(Ok(status)));
        return;
    }

    let error = with_tail(
        format!("Connection lost: ssh {}", describe(Ok(status))),
        &tail,
    );
    tracing::warn!("{error}");
}

async fn relay_stderr(stderr: ChildStderr, tail: StderrTail) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();

    while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
        let message = line.trim_end_matches(['\n', '\r']);
        tracing::warn!("ssh: {message}");

        if let Ok(mut tail) = tail.lock() {
            if tail.len() == STDERR_TAIL_LINES {
                tail.remove(0);
            }
            tail.push(message.to_string());
        }

        line.clear();
    }
}

fn describe(status: Result<ExitStatus, oneshot::error::RecvError>) -> String {
    match status {
        Ok(status) => match status.code() {
            Some(code) => format!("exited with code {code}"),
            None => "was terminated".to_string(),
        },
        Err(_) => "exited".to_string(),
    }
}

fn with_tail(message: String, tail: &Mutex<Vec<String>>) -> TransportError {
    let lines = tail.lock().unwrap_or_else(|err| err.into_inner());

    if lines.is_empty() {
        TransportError::Message(message)
    } else {
        TransportError::Message(format!("{message}\nssh stderr:\n{}", lines.join("\n")))
    }
}

/// Holds the token that keeps ssh alive, so that the child is killed once the
/// transport is dropped.
struct GuardedRead<R> {
    inner: R,
    _alive: oneshot::Sender<()>,
}

impl<R: AsyncRead + Unpin> AsyncRead for GuardedRead<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_host() {
        assert_eq!(
            parse_ssh_connect("host").unwrap(),
            ("host".to_string(), 3000)
        );
    }

    #[test]
    fn parse_user_at_host() {
        assert_eq!(
            parse_ssh_connect("user@host").unwrap(),
            ("user@host".to_string(), 3000)
        );
    }

    #[test]
    fn parse_explicit_port() {
        assert_eq!(
            parse_ssh_connect("host:4000").unwrap(),
            ("host".to_string(), 4000)
        );
        assert_eq!(
            parse_ssh_connect("user@host:4000").unwrap(),
            ("user@host".to_string(), 4000)
        );
    }

    #[test]
    fn parse_alias_with_dots() {
        assert_eq!(
            parse_ssh_connect("user@my.host.alias").unwrap(),
            ("user@my.host.alias".to_string(), 3000)
        );
        assert_eq!(
            parse_ssh_connect("my.host.alias:5000").unwrap(),
            ("my.host.alias".to_string(), 5000)
        );
    }

    #[test]
    fn parse_ipv6_literal() {
        assert_eq!(
            parse_ssh_connect("fe80::1").unwrap(),
            ("fe80::1".to_string(), 3000)
        );
        assert_eq!(
            parse_ssh_connect("[fe80::1]").unwrap(),
            ("fe80::1".to_string(), 3000)
        );
        assert_eq!(
            parse_ssh_connect("[fe80::1]:4000").unwrap(),
            ("fe80::1".to_string(), 4000)
        );
        assert_eq!(
            parse_ssh_connect("user@fe80::1").unwrap(),
            ("user@fe80::1".to_string(), 3000)
        );
        assert_eq!(
            parse_ssh_connect("user@[fe80::1]").unwrap(),
            ("user@fe80::1".to_string(), 3000)
        );
        assert_eq!(
            parse_ssh_connect("user@[fe80::1]:4000").unwrap(),
            ("user@fe80::1".to_string(), 4000)
        );
    }

    #[test]
    fn parse_malformed_empty() {
        assert!(matches!(
            parse_ssh_connect(""),
            Err(ClientError::InvalidRemoteHost)
        ));
    }

    #[test]
    fn parse_malformed_bad_port() {
        assert!(matches!(
            parse_ssh_connect("host:abc"),
            Err(ClientError::InvalidRemoteHost)
        ));
        assert!(matches!(
            parse_ssh_connect("host:99999"),
            Err(ClientError::InvalidRemoteHost)
        ));
    }

    #[test]
    fn parse_malformed_empty_destination_with_port() {
        assert!(matches!(
            parse_ssh_connect(":3000"),
            Err(ClientError::InvalidRemoteHost)
        ));
    }
}
