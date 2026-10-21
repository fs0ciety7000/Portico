//! Port forwarding (`-L`, `-R`; `-D` post-MVP).
//!
//! Each running forward is a task under its session's cancellation token;
//! state changes are published as [`crate::events::TunnelEvent`]s.
//!
//! - `-L`: listens locally (every address the bind name resolves to) and
//!   opens a `direct-tcpip` channel per accepted connection.
//! - `-R`: asks the server to listen (`tcpip-forward`) and registers a route;
//!   the client handler accepts a `forwarded-tcpip` channel **only** if it
//!   matches a route this session asked for, then connects to the local
//!   target. Every other server-opened channel is refused.
//! - Binding defaults to `localhost`; any other address must be explicit.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelOpenFailure};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::events::{EngineEvent, TunnelEvent};
use crate::ids::{SessionId, TunnelId};
use crate::logs::LogLevel;
use crate::session::{Attached, SessionCtx};

/// Simultaneous connections per tunnel (more are refused).
const MAX_CONNECTIONS: usize = 256;
/// Time allowed to open a channel or a local connection for one client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Data buffered from the SSH side towards a local peer (chunks).
const PIPE_QUEUE: usize = 128;
const PIPE_CHUNK: usize = 32 * 1024;
/// A local peer that accepts nothing for this long is disconnected: russh
/// has one session loop for every channel, so a stalled reader would
/// otherwise freeze the whole connection (see docs/STACK.md).
pub(crate) const STALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Shortest stall allowed, even with a very tight keepalive.
const MIN_STALL: Duration = Duration::from_millis(100);

/// Stall limit for a session: [`STALL_TIMEOUT`], shortened to half the
/// keepalive budget (`ServerAliveInterval` × `ServerAliveCountMax`) so a
/// stalled peer is cut before the frozen loop makes the session time out.
pub(crate) fn stall_limit(keepalive: Duration, count_max: u32, disconnects: bool) -> Duration {
    if !disconnects {
        return STALL_TIMEOUT;
    }
    let budget = keepalive.saturating_mul(count_max.max(1)) / 2;
    STALL_TIMEOUT.min(budget).max(MIN_STALL)
}
/// Pause after a failed `accept` (e.g. too many open files).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(200);

/// `[bind_address:]port`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForwardSpec {
    /// Listen locally, connect from the server to `target`.
    Local { bind: Endpoint, target: Endpoint },
    /// Ask the server to listen on `bind`, connect locally to `target`.
    Remote { bind: Endpoint, target: Endpoint },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid forward `{input}`: {reason}")]
pub struct ForwardParseError {
    pub input: String,
    pub reason: &'static str,
}

impl ForwardSpec {
    /// Parses OpenSSH `LocalForward` / `-L` syntax:
    /// `[bind_address:]port host:hostport` or `[bind_address:]port:host:hostport`.
    /// The bind address defaults to `localhost` (never `0.0.0.0` implicitly).
    pub fn parse_local(input: &str) -> Result<Self, ForwardParseError> {
        let (bind, target) = parse_pair(input)?;
        Ok(Self::Local { bind, target })
    }

    /// Same syntax as [`Self::parse_local`] for `RemoteForward` / `-R`.
    pub fn parse_remote(input: &str) -> Result<Self, ForwardParseError> {
        let (bind, target) = parse_pair(input)?;
        Ok(Self::Remote { bind, target })
    }

    /// What a user types in a UI: `-L 8080:db:5432`, `L 8080 db:5432`,
    /// `-R 9000:localhost:3000`.
    pub fn parse_cli(input: &str) -> Result<Self, ForwardParseError> {
        let text = input.trim();
        let err = |reason| ForwardParseError {
            input: text.to_owned(),
            reason,
        };
        let (kind, rest) = text
            .split_once(char::is_whitespace)
            .ok_or_else(|| err("start with -L or -R, e.g. -L 8080:db.internal:5432"))?;
        match kind.trim_start_matches('-') {
            "L" | "l" => Self::parse_local(rest.trim()),
            "R" | "r" => Self::parse_remote(rest.trim()),
            _ => Err(err("start with -L or -R")),
        }
    }
}

fn parse_pair(input: &str) -> Result<(Endpoint, Endpoint), ForwardParseError> {
    let err = |reason| ForwardParseError {
        input: input.to_owned(),
        reason,
    };
    let parts: Vec<String> = if input.split_whitespace().count() == 2 {
        input.split_whitespace().flat_map(split_colons).collect()
    } else {
        split_colons(input.trim())
    };
    let (bind_host, bind_port, host, port) = match parts.as_slice() {
        [bp, h, p] => ("localhost".to_owned(), bp, h, p),
        [bh, bp, h, p] => (bh.clone(), bp, h, p),
        _ => return Err(err("expected [bind:]port:host:hostport")),
    };
    // Bind port 0 asks for an ephemeral port (shown once listening).
    let bind_port = bind_port
        .parse::<u16>()
        .map_err(|_| err("invalid bind port"))?;
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)
        .ok_or_else(|| err("invalid target port"))?;
    let bind_host = if bind_host.is_empty() || bind_host == "*" {
        // OpenSSH: empty or `*` means all interfaces, explicitly requested.
        "0.0.0.0".to_owned()
    } else {
        bind_host
    };
    Ok((
        Endpoint {
            host: bind_host,
            port: bind_port,
        },
        Endpoint {
            host: host.clone(),
            port,
        },
    ))
}

/// Splits on `:` while keeping `[ipv6]` literals intact.
fn split_colons(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_brackets = false;
    for c in s.chars() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            ':' if !in_brackets => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

impl fmt::Display for ForwardSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { bind, target } => write!(f, "-L {bind} → {target}"),
            Self::Remote { bind, target } => write!(f, "-R {bind} → {target}"),
        }
    }
}

impl ForwardSpec {
    /// True when the listening side is reachable from other machines.
    pub fn is_exposed(&self) -> bool {
        let (Self::Local { bind, .. } | Self::Remote { bind, .. }) = self;
        !matches!(bind.host.as_str(), "localhost" | "127.0.0.1" | "::1")
    }
}

/// A running tunnel, as listed to the UIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelInfo {
    pub id: TunnelId,
    pub session: SessionId,
    pub spec: ForwardSpec,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TunnelError {
    #[error("cannot listen on {bind}: {source}")]
    Bind {
        bind: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the server refused to listen on {bind}")]
    RemoteRefused { bind: String },
    #[error("unknown or stopped tunnel {0}")]
    UnknownTunnel(TunnelId),
    #[error("{bind} is reachable from other machines: confirmation required")]
    ExposedNotConfirmed { bind: String },
}

impl TunnelError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Bind { source, .. } => match source.kind() {
                std::io::ErrorKind::AddrInUse => (
                    "port_in_use",
                    Some(
                        "Another program already listens on this port: choose another local port.",
                    ),
                ),
                std::io::ErrorKind::PermissionDenied => (
                    "port_forbidden",
                    Some("Ports below 1024 need privileges: choose a port above 1024."),
                ),
                _ => ("bind_failed", None),
            },
            Self::RemoteRefused { .. } => (
                "remote_forward_refused",
                Some(
                    "The server refuses remote forwarding (AllowTcpForwarding, GatewayPorts) or the port is taken.",
                ),
            ),
            Self::UnknownTunnel(_) => ("unknown_tunnel", None),
            Self::ExposedNotConfirmed { .. } => (
                "tunnel_exposed",
                Some(
                    "This listens on all interfaces: confirm to expose it to the network, or bind to localhost.",
                ),
            ),
        }
    }
}

/// A `-R` route this session asked the server for.
#[derive(Debug, Clone)]
pub(crate) struct RemoteRoute {
    tunnel: TunnelId,
    target: Endpoint,
    cancel: CancellationToken,
    slots: Arc<Semaphore>,
}

/// Routes by the (address, port) the server listens on.
pub(crate) type RemoteRoutes = Arc<Mutex<HashMap<(String, u32), RemoteRoute>>>;

fn emit(ctx: &SessionCtx, id: TunnelId, event: TunnelEvent) {
    ctx.engine.emit(EngineEvent::Tunnel {
        id,
        session: ctx.id,
        event,
    });
}

/// Runs a tunnel until `cancel` (or the session) ends it.
pub(crate) async fn run(
    attached: Attached,
    id: TunnelId,
    spec: ForwardSpec,
    cancel: CancellationToken,
) {
    let ctx = attached.ctx.clone();
    let result = match &spec {
        ForwardSpec::Local { bind, target } => {
            run_local(&attached, id, bind, target, &cancel).await
        }
        ForwardSpec::Remote { bind, target } => {
            run_remote(&attached, id, bind, target, &cancel).await
        }
    };
    match result {
        Ok(()) => {
            ctx.log(LogLevel::Info, "tunnel", format!("{spec} stopped"));
            emit(&ctx, id, TunnelEvent::Stopped);
        }
        Err(e) => {
            ctx.log(LogLevel::Warn, "tunnel", format!("{spec}: {e}"));
            emit(&ctx, id, TunnelEvent::Failed { error: e.summary() });
        }
    }
    ctx.engine.forget_tunnel(id);
}

/// Listens on every address `bind` resolves to, all on the same port.
async fn bind_all(ctx: &SessionCtx, bind: &Endpoint) -> crate::Result<Vec<TcpListener>> {
    let bind_error = |source| TunnelError::Bind {
        bind: bind.to_string(),
        source,
    };
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bind.host.as_str(), bind.port))
        .await
        .map_err(bind_error)?
        .collect();
    let mut listeners = Vec::new();
    let mut last_error = None;
    let mut port = bind.port;
    for mut addr in addrs {
        // With port 0, every address family gets the port chosen first.
        addr.set_port(port);
        match TcpListener::bind(addr).await {
            Ok(l) => {
                if port == 0 {
                    port = l.local_addr().map_or(0, |a| a.port());
                }
                listeners.push(l);
            }
            Err(e) => {
                if !listeners.is_empty() || last_error.is_none() {
                    ctx.log(
                        LogLevel::Warn,
                        "tunnel",
                        format!("-L cannot listen on {addr}: {e}"),
                    );
                }
                last_error = Some(e);
            }
        }
    }
    if listeners.is_empty() {
        return Err(bind_error(last_error.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no address to bind")
        }))
        .into());
    }
    Ok(listeners)
}

async fn run_local(
    attached: &Attached,
    id: TunnelId,
    bind: &Endpoint,
    target: &Endpoint,
    cancel: &CancellationToken,
) -> crate::Result<()> {
    let ctx = &attached.ctx;
    let listeners = bind_all(ctx, bind).await?;
    let shown = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    ctx.log(
        LogLevel::Info,
        "tunnel",
        format!("-L listening on {shown} → {target}"),
    );
    emit(ctx, id, TunnelEvent::Listening { bind: shown });

    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (accepted_tx, mut accepted) = tokio::sync::mpsc::channel::<(TcpStream, SocketAddr)>(16);
    for listener in listeners {
        let tx = accepted_tx.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let next = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    next = listener.accept() => next,
                };
                match next {
                    Ok(conn) => {
                        if tx.send(conn).await.is_err() {
                            return;
                        }
                    }
                    // Out of file descriptors and the like: do not spin.
                    Err(_) => tokio::time::sleep(ACCEPT_BACKOFF).await,
                }
            }
        });
    }
    drop(accepted_tx);
    loop {
        let (socket, peer) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            next = accepted.recv() => match next {
                Some(conn) => conn,
                None => return Ok(()),
            },
        };
        let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
            ctx.log(
                LogLevel::Warn,
                "tunnel",
                format!("{peer} refused: too many connections"),
            );
            continue;
        };
        tokio::spawn(serve_local(
            attached.clone(),
            id,
            target.clone(),
            peer,
            socket,
            permit,
            cancel.child_token(),
        ));
    }
}

/// One `-L` connection: opens `direct-tcpip` to the target, then pipes.
async fn serve_local(
    attached: Attached,
    id: TunnelId,
    target: Endpoint,
    peer: SocketAddr,
    socket: TcpStream,
    _permit: tokio::sync::OwnedSemaphorePermit,
    cancel: CancellationToken,
) {
    let ctx = &attached.ctx;
    let opened = tokio::time::timeout(
        CONNECT_TIMEOUT,
        attached.handle.channel_open_direct_tcpip(
            target.host.clone(),
            u32::from(target.port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        ),
    )
    .await;
    let channel = match opened {
        Ok(Ok(channel)) => channel,
        Ok(Err(e)) => {
            ctx.log(LogLevel::Warn, "tunnel", format!("{peer} → {target}: {e}"));
            return;
        }
        Err(_) => {
            ctx.log(
                LogLevel::Warn,
                "tunnel",
                format!("{peer} → {target}: timed out"),
            );
            return;
        }
    };
    pipe(ctx, id, peer.to_string(), channel, socket, &cancel).await;
}

async fn run_remote(
    attached: &Attached,
    id: TunnelId,
    bind: &Endpoint,
    target: &Endpoint,
    cancel: &CancellationToken,
) -> crate::Result<()> {
    let ctx = &attached.ctx;
    let route = RemoteRoute {
        tunnel: id,
        target: target.clone(),
        cancel: cancel.clone(),
        slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
    };
    let routes = &attached.routes;
    let register = |port: u32| {
        routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((bind.host.clone(), port), route.clone());
    };
    let unregister = |port: u32| {
        routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(bind.host.clone(), port));
    };
    // A fixed port is routed before asking, so a connection arriving right
    // after the server's reply is not refused.
    if bind.port != 0 {
        register(u32::from(bind.port));
    }
    let requested = tokio::time::timeout(
        CONNECT_TIMEOUT,
        attached
            .handle
            .tcpip_forward(bind.host.clone(), u32::from(bind.port)),
    )
    .await;
    let port = match requested {
        Ok(Ok(port)) => port,
        Ok(Err(_)) => {
            unregister(u32::from(bind.port));
            return Err(TunnelError::RemoteRefused {
                bind: bind.to_string(),
            }
            .into());
        }
        Err(_) => {
            unregister(u32::from(bind.port));
            // The server may still accept later: withdraw the request.
            let handle = Arc::clone(&attached.handle);
            let host = bind.host.clone();
            let port = u32::from(bind.port);
            tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(CONNECT_TIMEOUT, handle.cancel_tcpip_forward(host, port))
                        .await;
            });
            return Err(crate::Error::Timeout {
                phase: "asking the server to listen on",
                target: bind.to_string(),
            });
        }
    };
    // Port 0 asks the server to choose; it then reports the port.
    let port = if bind.port == 0 {
        register(port);
        port
    } else {
        u32::from(bind.port)
    };
    let key = (bind.host.clone(), port);
    let shown = Endpoint {
        host: bind.host.clone(),
        port: u16::try_from(port).unwrap_or(bind.port),
    };
    ctx.log(
        LogLevel::Info,
        "tunnel",
        format!("-R server listens on {shown} → {target}"),
    );
    emit(
        ctx,
        id,
        TunnelEvent::Listening {
            bind: shown.to_string(),
        },
    );

    cancel.cancelled().await;
    attached
        .routes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&key);
    // Best effort: the session may already be gone.
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        attached
            .handle
            .cancel_tcpip_forward(bind.host.clone(), port),
    )
    .await;
    Ok(())
}

/// Called by the client handler for a server-opened `forwarded-tcpip`
/// channel. Unknown routes are refused.
pub(crate) fn serve_forwarded(
    ctx: &SessionCtx,
    routes: &RemoteRoutes,
    channel: Channel<Msg>,
    reply: russh::client::ChannelOpenHandle,
    connected: (&str, u32),
    originator: (&str, u32),
) {
    let route = {
        let routes = routes.lock().unwrap_or_else(PoisonError::into_inner);
        routes
            .get(&(connected.0.to_owned(), connected.1))
            // Some servers report the address they actually bound.
            .or_else(|| {
                let mut same_port = routes.iter().filter(|((_, p), _)| *p == connected.1);
                match (same_port.next(), same_port.next()) {
                    (Some((_, r)), None) => Some(r),
                    _ => None,
                }
            })
            .cloned()
    };
    let Some(route) = route else {
        ctx.log(
            LogLevel::Warn,
            "tunnel",
            format!(
                "refused a forwarded connection to {}:{} that no tunnel asked for",
                crate::sftp::display_safe(connected.0, 64),
                connected.1
            ),
        );
        tokio::spawn(reply.reject(ChannelOpenFailure::AdministrativelyProhibited));
        return;
    };
    let Ok(permit) = Arc::clone(&route.slots).try_acquire_owned() else {
        tokio::spawn(reply.reject(ChannelOpenFailure::ResourceShortage));
        return;
    };
    let ctx = ctx.clone();
    // Chosen by the server: shown in UIs, so made display-safe.
    let peer = format!(
        "{}:{}",
        crate::sftp::display_safe(originator.0, 64),
        originator.1
    );
    tokio::spawn(async move {
        let _permit = permit;
        let target = route.target.to_socket_target();
        let socket = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect(target.as_str()),
        )
        .await
        {
            Ok(Ok(socket)) => socket,
            Ok(Err(e)) => {
                ctx.log(
                    LogLevel::Warn,
                    "tunnel",
                    format!("{peer} → {}: {e}", route.target),
                );
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
                return;
            }
            Err(_) => {
                ctx.log(
                    LogLevel::Warn,
                    "tunnel",
                    format!("{peer} → {}: timed out", route.target),
                );
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
                return;
            }
        };
        reply.accept().await;
        let cancel = route.cancel.child_token();
        pipe(&ctx, route.tunnel, peer, channel, socket, &cancel).await;
    });
}

/// Copies both ways until both sides are done, the tunnel stops, or the
/// local peer stalls. SSH → peer data goes through a bounded queue drained
/// by its own task, so russh's session loop never waits on a slow peer for
/// more than the session's [`stall_limit`].
async fn pipe(
    ctx: &SessionCtx,
    id: TunnelId,
    peer: String,
    channel: Channel<Msg>,
    socket: TcpStream,
    cancel: &CancellationToken,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    emit(
        ctx,
        id,
        TunnelEvent::ConnectionOpened { peer: peer.clone() },
    );
    let (mut ssh_read, mut ssh_write) = tokio::io::split(channel.into_stream());
    let (mut peer_read, mut peer_write) = socket.into_split();
    let (queue, mut queued) = tokio::sync::mpsc::channel::<bytes::Bytes>(PIPE_QUEUE);
    let stall = ctx.stall_limit();

    let from_ssh = async move {
        loop {
            let mut buf = bytes::BytesMut::with_capacity(PIPE_CHUNK);
            if ssh_read.read_buf(&mut buf).await? == 0 {
                return Ok::<_, std::io::Error>(());
            }
            match tokio::time::timeout(stall, queue.send(buf.freeze())).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Ok(()),
                Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the local peer stopped reading",
                    ));
                }
            }
        }
    };
    let to_peer = async move {
        while let Some(chunk) = queued.recv().await {
            peer_write.write_all(&chunk).await?;
        }
        peer_write.shutdown().await
    };
    let to_ssh = async move {
        tokio::io::copy(&mut peer_read, &mut ssh_write).await?;
        ssh_write.shutdown().await
    };
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(((), (), ())),
        r = async { tokio::try_join!(from_ssh, to_peer, to_ssh) } => r,
    };
    if let Err(e) = outcome
        && e.kind() == std::io::ErrorKind::TimedOut
    {
        ctx.log(
            LogLevel::Warn,
            "tunnel",
            format!("{peer} stopped reading: connection closed to keep the session responsive"),
        );
    }
    emit(ctx, id, TunnelEvent::ConnectionClosed { peer });
}

impl Endpoint {
    /// `host:port` for `TcpStream::connect` (IPv6 literals bracketed).
    fn to_socket_target(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(h: &str, p: u16) -> Endpoint {
        Endpoint {
            host: h.into(),
            port: p,
        }
    }

    #[test]
    fn parses_what_users_type() {
        assert!(matches!(
            ForwardSpec::parse_cli("-L 8080:db:5432"),
            Ok(ForwardSpec::Local { .. })
        ));
        assert!(matches!(
            ForwardSpec::parse_cli(" L 8080 db:5432"),
            Ok(ForwardSpec::Local { .. })
        ));
        assert!(matches!(
            ForwardSpec::parse_cli("-R 9000:localhost:3000"),
            Ok(ForwardSpec::Remote { .. })
        ));
        assert!(ForwardSpec::parse_cli("8080:db:5432").is_err());
        assert!(ForwardSpec::parse_cli("-D 1080").is_err());
        assert!(ForwardSpec::parse_cli("-L nope").is_err());
    }

    #[test]
    fn stall_limit_stays_under_the_keepalive_budget() {
        let s = Duration::from_secs;
        assert_eq!(stall_limit(s(15), 3, true), STALL_TIMEOUT);
        assert_eq!(stall_limit(s(1), 1, true), Duration::from_millis(500));
        assert_eq!(stall_limit(Duration::from_millis(50), 1, true), MIN_STALL);
        assert_eq!(stall_limit(s(1), 0, true), Duration::from_millis(500));
        // ServerAliveInterval 0: silence never disconnects.
        assert_eq!(stall_limit(s(1), 1, false), STALL_TIMEOUT);
    }

    #[test]
    fn parses_cli_and_config_forms() {
        let expected = ForwardSpec::Local {
            bind: ep("localhost", 8080),
            target: ep("db", 5432),
        };
        assert_eq!(ForwardSpec::parse_local("8080:db:5432").unwrap(), expected);
        assert_eq!(ForwardSpec::parse_local("8080 db:5432").unwrap(), expected);
        assert_eq!(
            ForwardSpec::parse_local("127.0.0.1:8080:[::1]:80").unwrap(),
            ForwardSpec::Local {
                bind: ep("127.0.0.1", 8080),
                target: ep("::1", 80)
            }
        );
        assert_eq!(
            ForwardSpec::parse_remote("*:9000:localhost:3000").unwrap(),
            ForwardSpec::Remote {
                bind: ep("0.0.0.0", 9000),
                target: ep("localhost", 3000)
            }
        );
        assert!(ForwardSpec::parse_local("8080").is_err());
        assert!(
            ForwardSpec::parse_local("0:db:1").is_ok(),
            "ephemeral bind port"
        );
        assert!(ForwardSpec::parse_local("8080:db:0").is_err());
    }

    #[test]
    fn exposure_and_display() {
        let local = ForwardSpec::parse_local("8080:db:5432").unwrap();
        assert!(!local.is_exposed());
        assert_eq!(local.to_string(), "-L localhost:8080 → db:5432");
        assert!(
            ForwardSpec::parse_remote("*:9000:localhost:3000")
                .unwrap()
                .is_exposed()
        );
        assert_eq!(ep("::1", 22).to_string(), "[::1]:22");
    }
}
