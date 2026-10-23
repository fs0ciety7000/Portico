//! Port forwarding (`-L`, `-R`, `-D`).
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
//! - `-D`: listens locally like `-L` and speaks SOCKS5 ([`crate::socks`]) to
//!   each client; the destination it asks for (a domain is resolved by the
//!   server, never here) becomes the `direct-tcpip` target.
//! - Binding defaults to `localhost`; any other address must be explicit.
//!
//! Invariants:
//! - a `direct-tcpip` open is never dropped mid-flight: when Portico stops
//!   waiting (timeout, stop), a confirmation that still comes is closed
//!   ([`open_direct_tcpip`]), so no channel stays open on the server;
//! - data from the server waiting for slow local peers is bounded per session
//!   ([`ForwardBudget`]); a peer is cut only when that budget is exhausted and
//!   it accepted nothing for the session's [`stall_limit`];
//! - per-connection refusals and failures are logged at a bounded rate, and
//!   SOCKS destinations are logged only when they fail.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelOpenFailure};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::events::{EngineEvent, TunnelEvent};
use crate::ids::{SessionId, TunnelId};
use crate::logs::LogLevel;
use crate::session::{Attached, SessionCtx};

/// Simultaneous connections per tunnel (more are refused).
const MAX_CONNECTIONS: usize = 256;
/// Time allowed to open a local connection for one client (`-R`), or to
/// ask the server to listen. Channel opens use the session's
/// `ConnectTimeout` ([`SessionCtx::channel_open_timeout`]).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an abandoned `direct-tcpip` open is still awaited, so that a
/// late confirmation can be closed instead of leaking a server channel.
const REAP_WINDOW: Duration = Duration::from_secs(120);
/// Largest read from the SSH side.
const PIPE_CHUNK: usize = 32 * 1024;
/// Bytes from the server buffered towards local peers, per session (every
/// tunnel together): a paused reader (video, slow consumer) keeps its
/// connection while the session as a whole stays under this.
const SESSION_BUFFER_BUDGET: usize = 64 * 1024 * 1024;
/// Each buffered chunk costs at least this much budget, which also bounds
/// the number of chunks.
const MIN_CHUNK_CHARGE: usize = 1024;
/// Chunks one connection may queue: never reached before the byte budget.
const MAX_QUEUED_CHUNKS: usize = SESSION_BUFFER_BUDGET / MIN_CHUNK_CHARGE;
/// A local peer that accepts nothing for this long while the session's
/// buffer budget is exhausted is disconnected: russh has one session loop
/// for every channel, so it would otherwise freeze the whole connection
/// (see docs/STACK.md).
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
/// Time a SOCKS client has to send its greeting and request.
const SOCKS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a refused SOCKS client may keep sending before it is closed.
const SOCKS_LINGER: Duration = Duration::from_millis(500);
/// Pause after a failed `accept` (e.g. too many open files).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(200);
/// SOCKS clients answered "general failure" at once when a `-D` tunnel is
/// full (more are closed bare), and the time each one gets.
const BUSY_REPLY_SLOTS: usize = 16;
const BUSY_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// Rate limit of per-connection logs, per tunnel: within one window, the
/// first refusal ("too many connections") and the first few failures
/// (handshake, channel open) are logged; the rest are counted and reported
/// in one line (a flood would otherwise evict useful logs and make the UIs
/// lag behind the event bus).
const LOG_WINDOW: Duration = Duration::from_secs(1);
const REFUSALS_LOGGED_PER_WINDOW: u32 = 1;
const FAILURES_LOGGED_PER_WINDOW: u32 = 5;

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
    /// Listen locally as a SOCKS5 proxy; each client picks its destination,
    /// reached from the server.
    Dynamic { bind: Endpoint },
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

    /// Parses OpenSSH `DynamicForward` / `-D` syntax: `[bind_address:]port`
    /// (IPv6 bind addresses in brackets), or OpenSSH's alternative
    /// `bind_address/port` (which needs no brackets: `::1/1080`). Like `-L`,
    /// the bind address defaults to `localhost`; empty or `*` means every
    /// IPv4 interface.
    pub fn parse_dynamic(input: &str) -> Result<Self, ForwardParseError> {
        let text = input.trim();
        let err = |reason| ForwardParseError {
            input: text.to_owned(),
            reason,
        };
        let parts = match text.split_once('/') {
            Some((host, port)) => {
                if port.contains('/') || host.contains(['[', ']']) {
                    return Err(err("expected [bind:]port, e.g. 1080"));
                }
                vec![host.to_owned(), port.to_owned()]
            }
            None => split_colons(text),
        };
        let (host, port) = match parts.as_slice() {
            [port] => ("localhost", port),
            [host, port] => (host.as_str(), port),
            _ => return Err(err("expected [bind:]port, e.g. 1080")),
        };
        if host.chars().any(char::is_whitespace) {
            return Err(err("expected [bind:]port, e.g. 1080"));
        }
        let port = port.parse::<u16>().map_err(|_| err("invalid bind port"))?;
        let host = if host.is_empty() || host == "*" {
            "0.0.0.0"
        } else {
            host
        };
        Ok(Self::Dynamic {
            bind: Endpoint {
                host: host.to_owned(),
                port,
            },
        })
    }

    /// What a user types in a UI: `-L 8080:db:5432`, `L 8080 db:5432`,
    /// `-R 9000:localhost:3000`, `-D 1080`.
    pub fn parse_cli(input: &str) -> Result<Self, ForwardParseError> {
        let text = input.trim();
        let err = |reason| ForwardParseError {
            input: text.to_owned(),
            reason,
        };
        let (kind, rest) = text
            .split_once(char::is_whitespace)
            .ok_or_else(|| err("start with -L, -R or -D, e.g. -L 8080:db.internal:5432"))?;
        match kind.trim_start_matches('-') {
            "L" | "l" => Self::parse_local(rest.trim()),
            "R" | "r" => Self::parse_remote(rest.trim()),
            "D" | "d" => Self::parse_dynamic(rest.trim()),
            _ => Err(err("start with -L, -R or -D")),
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
            Self::Dynamic { bind } => write!(f, "SOCKS · {bind}"),
        }
    }
}

impl ForwardSpec {
    /// Where the tunnel listens (locally for `-L` / `-D`, on the server
    /// for `-R`).
    pub fn bind(&self) -> &Endpoint {
        let (Self::Local { bind, .. } | Self::Remote { bind, .. } | Self::Dynamic { bind }) = self;
        bind
    }

    /// True when the listening side is reachable from other machines.
    pub fn is_exposed(&self) -> bool {
        !matches!(self.bind().host.as_str(), "localhost" | "127.0.0.1" | "::1")
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
            run_listener(&attached, id, bind, Some(target), &cancel).await
        }
        ForwardSpec::Dynamic { bind } => run_listener(&attached, id, bind, None, &cancel).await,
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
async fn bind_all(
    ctx: &SessionCtx,
    flag: &str,
    bind: &Endpoint,
) -> crate::Result<Vec<TcpListener>> {
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
                        format!("{flag} cannot listen on {addr}: {e}"),
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

/// Per-connection log lines of one tunnel, rate-limited ([`LOG_WINDOW`]).
#[derive(Debug)]
struct Throttle {
    /// Lines logged per window; the rest are counted.
    per_window: u32,
    /// How the counted lines are reported, e.g. "connections refused: too
    /// many connections" (prefixed with their number).
    summary: &'static str,
    state: Mutex<ThrottleState>,
}

#[derive(Debug, Default)]
struct ThrottleState {
    window_start: Option<tokio::time::Instant>,
    logged: u32,
    suppressed: u64,
}

impl Throttle {
    fn new(per_window: u32, summary: &'static str) -> Self {
        Self {
            per_window,
            summary,
            state: Mutex::new(ThrottleState::default()),
        }
    }

    /// Logs `message` (built only when it is logged) as a warning, or counts
    /// it once this window's quota is used.
    fn warn(&self, ctx: &SessionCtx, message: impl FnOnce() -> String) {
        let now = tokio::time::Instant::now();
        let (log_it, report) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut report = 0;
            if state
                .window_start
                .is_none_or(|start| now.duration_since(start) >= LOG_WINDOW)
            {
                report = std::mem::take(&mut state.suppressed);
                state.window_start = Some(now);
                state.logged = 0;
            }
            if state.logged < self.per_window {
                state.logged = state.logged.saturating_add(1);
                (true, report)
            } else {
                state.suppressed = state.suppressed.saturating_add(1);
                (false, report)
            }
        };
        self.report(ctx, report);
        if log_it {
            ctx.log(LogLevel::Warn, "tunnel", message());
        }
    }

    /// Reports what is still counted (when the tunnel ends).
    fn flush(&self, ctx: &SessionCtx) {
        let report = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .suppressed,
        );
        self.report(ctx, report);
    }

    fn report(&self, ctx: &SessionCtx, count: u64) {
        if count > 0 {
            ctx.log(
                LogLevel::Warn,
                "tunnel",
                format!("{count} {}", self.summary),
            );
        }
    }
}

/// Rate-limited logs shared by a listening tunnel and its connections.
#[derive(Debug)]
struct ListenerLogs {
    /// "too many connections"
    refusals: Throttle,
    /// SOCKS handshakes and channel opens that failed.
    failures: Throttle,
}

impl ListenerLogs {
    fn new(flag: &str) -> Self {
        let (refused, failed) = if flag == "-D" {
            (
                "connections refused (-D): too many connections",
                "more -D connection failures (not logged one by one)",
            )
        } else {
            (
                "connections refused (-L): too many connections",
                "more -L connection failures (not logged one by one)",
            )
        };
        Self {
            refusals: Throttle::new(REFUSALS_LOGGED_PER_WINDOW, refused),
            failures: Throttle::new(FAILURES_LOGGED_PER_WINDOW, failed),
        }
    }
}

/// `-L` (`target` set) or `-D` (`None`: each client names its target over
/// SOCKS5): listens, then serves every accepted connection on its own task.
async fn run_listener(
    attached: &Attached,
    id: TunnelId,
    bind: &Endpoint,
    target: Option<&Endpoint>,
    cancel: &CancellationToken,
) -> crate::Result<()> {
    let ctx = &attached.ctx;
    let flag = if target.is_some() { "-L" } else { "-D" };
    let listeners = bind_all(ctx, flag, bind).await?;
    let shown = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let to = target.map_or_else(|| "SOCKS5".to_owned(), ToString::to_string);
    ctx.log(
        LogLevel::Info,
        "tunnel",
        format!("{flag} listening on {shown} → {to}"),
    );
    emit(ctx, id, TunnelEvent::Listening { bind: shown });

    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let busy_slots = Arc::new(Semaphore::new(BUSY_REPLY_SLOTS));
    let logs = Arc::new(ListenerLogs::new(flag));
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
            () = cancel.cancelled() => break,
            next = accepted.recv() => match next {
                Some(conn) => conn,
                None => break,
            },
        };
        let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
            logs.refusals.warn(ctx, || {
                format!("{flag} {peer} refused: too many connections")
            });
            // A SOCKS client is told why (bounded: its own few slots, a
            // short timeout); anything else is closed bare.
            if target.is_none()
                && let Ok(slot) = Arc::clone(&busy_slots).try_acquire_owned()
            {
                tokio::spawn(refuse_busy(socket, slot, cancel.child_token()));
            }
            continue;
        };
        match target {
            Some(target) => tokio::spawn(serve_local(
                attached.clone(),
                id,
                target.clone(),
                peer,
                socket,
                permit,
                Arc::clone(&logs),
                cancel.child_token(),
            )),
            None => tokio::spawn(serve_dynamic(
                attached.clone(),
                id,
                peer,
                socket,
                permit,
                Arc::clone(&logs),
                cancel.child_token(),
            )),
        };
    }
    logs.refusals.flush(ctx);
    logs.failures.flush(ctx);
    Ok(())
}

/// How a `direct-tcpip` open ended, as seen by the connection that asked.
enum Opened {
    Channel(Channel<Msg>),
    Refused(russh::Error),
    TimedOut,
    Cancelled,
}

/// Opens `direct-tcpip` to `target`, waiting at most the session's
/// `ConnectTimeout` and never past `cancel`.
///
/// russh registers a channel when its confirmation arrives, even if the
/// future that asked was dropped, and nothing would ever close it. So the
/// open runs on its own task, which is never dropped mid-flight: when the
/// caller gives up, a confirmation arriving later (within [`REAP_WINDOW`]) is
/// closed at once.
async fn open_direct_tcpip(
    attached: &Attached,
    target: &Endpoint,
    peer: SocketAddr,
    cancel: &CancellationToken,
) -> Opened {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let handle = Arc::clone(&attached.handle);
    let (host, port) = (target.host.clone(), u32::from(target.port));
    let (origin, origin_port) = (peer.ip().to_string(), u32::from(peer.port()));
    tokio::spawn(async move {
        let opening = handle.channel_open_direct_tcpip(host, port, origin, origin_port);
        let Ok(result) = tokio::time::timeout(REAP_WINDOW, opening).await else {
            return;
        };
        // The caller gave up: close what the server just opened.
        if let Err(Ok(channel)) = tx.send(result) {
            let _ = channel.close().await;
        }
    });
    let limit = attached.ctx.channel_open_timeout();
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        r = tokio::time::timeout(limit, &mut rx) => r.ok(),
    };
    match outcome {
        Some(Ok(Ok(channel))) => Opened::Channel(channel),
        Some(Ok(Err(e))) => Opened::Refused(e),
        // The opening task ended without an answer (session gone).
        Some(Err(_)) => Opened::Refused(russh::Error::Disconnect),
        None => {
            // From here on the opening task sees the closed receiver and
            // closes a late channel itself; one that arrived just before is
            // closed here.
            rx.close();
            if let Ok(Ok(channel)) = rx.try_recv() {
                tokio::spawn(async move {
                    let _ = channel.close().await;
                });
            }
            if cancel.is_cancelled() {
                Opened::Cancelled
            } else {
                Opened::TimedOut
            }
        }
    }
}

/// One `-L` connection: opens `direct-tcpip` to the target, then pipes.
#[allow(clippy::too_many_arguments)] // one connection's whole context, spawned as is
async fn serve_local(
    attached: Attached,
    id: TunnelId,
    target: Endpoint,
    peer: SocketAddr,
    socket: TcpStream,
    _permit: OwnedSemaphorePermit,
    logs: Arc<ListenerLogs>,
    cancel: CancellationToken,
) {
    let ctx = &attached.ctx;
    let channel = match open_direct_tcpip(&attached, &target, peer, &cancel).await {
        Opened::Channel(channel) => channel,
        Opened::Refused(e) => {
            logs.failures
                .warn(ctx, || format!("-L {peer} → {target}: {e}"));
            return;
        }
        Opened::TimedOut => {
            logs.failures
                .warn(ctx, || format!("-L {peer} → {target}: timed out"));
            return;
        }
        Opened::Cancelled => return,
    };
    pipe(ctx, id, peer.to_string(), channel, socket, &cancel).await;
}

/// One `-D` connection: SOCKS5 handshake (bounded in time), `direct-tcpip`
/// to the destination the client named, the matching reply, then pipes.
///
/// Destinations are browsing history: a connection that succeeds is not
/// logged with its destination (the `ConnectionOpened` event names the
/// local peer only); failures are, so they can be diagnosed.
async fn serve_dynamic(
    attached: Attached,
    id: TunnelId,
    peer: SocketAddr,
    mut socket: TcpStream,
    _permit: OwnedSemaphorePermit,
    logs: Arc<ListenerLogs>,
    cancel: CancellationToken,
) {
    use crate::socks::{Reply, SocksError};

    let ctx = &attached.ctx;
    let negotiated = tokio::select! {
        biased;
        () = cancel.cancelled() => return,
        r = tokio::time::timeout(SOCKS_HANDSHAKE_TIMEOUT, crate::socks::negotiate(&mut socket)) => {
            r.unwrap_or(Err(SocksError::Timeout))
        }
    };
    let target = match negotiated {
        Ok(target) => target,
        Err(e) => {
            logs.failures.warn(ctx, || format!("-D {peer}: {e}"));
            if !matches!(e, SocksError::Timeout) {
                close_gently(&mut socket, &cancel).await;
            }
            return;
        }
    };
    let refusal = match open_direct_tcpip(&attached, &target, peer, &cancel).await {
        Opened::Channel(channel) => {
            if crate::socks::reply(&mut socket, Reply::Succeeded)
                .await
                .is_ok()
            {
                pipe(ctx, id, peer.to_string(), channel, socket, &cancel).await;
            } else {
                // The client left before the answer: `Channel` does not
                // close itself when dropped.
                let _ = channel.close().await;
            }
            return;
        }
        Opened::Refused(e) => {
            let why = refusal_reason(&target, &e);
            logs.failures.warn(ctx, || {
                format!("-D {peer} → {}: {why}", shown_target(&target))
            });
            socks_reply_for(&e)
        }
        Opened::TimedOut => {
            logs.failures.warn(ctx, || {
                format!("-D {peer} → {}: timed out", shown_target(&target))
            });
            Reply::HostUnreachable
        }
        Opened::Cancelled => return,
    };
    // Best effort: the client may be gone already.
    if crate::socks::reply(&mut socket, refusal).await.is_ok() {
        close_gently(&mut socket, &cancel).await;
    }
}

/// A SOCKS destination for logs: named by a local program, so made
/// display-safe; IPv6 literals in brackets.
fn shown_target(target: &Endpoint) -> Endpoint {
    Endpoint {
        host: crate::sftp::display_safe(&target.host, 255).into_owned(),
        port: target.port,
    }
}

/// Why the server refused a `-D` destination, for the log. sshd reports a
/// closed port and a name it cannot resolve alike (`ConnectFailed`).
fn refusal_reason(target: &Endpoint, error: &russh::Error) -> String {
    match error {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
            if target.host.parse::<std::net::IpAddr>().is_ok() {
                "refused or unreachable from the server".to_owned()
            } else {
                "refused or unresolvable on the server".to_owned()
            }
        }
        other => other.to_string(),
    }
}

/// Answers a SOCKS client that arrived while the tunnel is full: the
/// handshake goes on until the request, which gets "general failure", so
/// the client reports an error instead of a dropped connection.
async fn refuse_busy(
    mut socket: TcpStream,
    _slot: OwnedSemaphorePermit,
    cancel: CancellationToken,
) {
    use crate::socks::{Reply, SocksError};

    let refuse = async {
        crate::socks::negotiate(&mut socket).await?;
        crate::socks::reply(&mut socket, Reply::GeneralFailure).await?;
        Ok::<_, SocksError>(())
    };
    let answered = tokio::select! {
        biased;
        () = cancel.cancelled() => return,
        r = tokio::time::timeout(BUSY_REPLY_TIMEOUT, refuse) => r,
    };
    // Answered (refused after the request, or during the handshake):
    // close so the client reads the reply. Timeouts and I/O errors: bare.
    let replied = match answered {
        Ok(Ok(())) => true,
        Ok(Err(e)) => !matches!(e, SocksError::Io(_)),
        Err(_) => false,
    };
    if replied {
        close_gently(&mut socket, &cancel).await;
    }
}

/// Closes after a SOCKS refusal so the client reads the reply: sends FIN,
/// then discards what the client still sends for a short, bounded time
/// (closing with unread input would reset the connection and lose the
/// reply).
async fn close_gently(socket: &mut TcpStream, cancel: &CancellationToken) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if socket.shutdown().await.is_err() {
        return;
    }
    let mut sink = [0_u8; 512];
    let drain = async { while matches!(socket.read(&mut sink).await, Ok(n) if n > 0) {} };
    tokio::select! {
        biased;
        () = cancel.cancelled() => {}
        _ = tokio::time::timeout(SOCKS_LINGER, drain) => {}
    }
}

/// SOCKS reply for a `direct-tcpip` the server refused. OpenSSH reports a
/// refused port and an unknown or unreachable host alike (`ConnectFailed`):
/// "connection refused" is what the client shows for both.
fn socks_reply_for(error: &russh::Error) -> crate::socks::Reply {
    use crate::socks::Reply;
    match error {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
            Reply::ConnectionRefused
        }
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited) => {
            Reply::NotAllowed
        }
        _ => Reply::GeneralFailure,
    }
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

/// Bytes from the server buffered towards local tunnel peers, for one
/// session ([`SESSION_BUFFER_BUDGET`], shared by all its tunnels).
///
/// russh reopens a channel's window when data is *received*, not when it is
/// consumed, and serves every channel from one loop that must never wait
/// on a local peer: the only backpressure left is this budget. While it
/// lasts, a paused reader keeps its connection; once a chunk cannot be
/// reserved, the session loop is waiting, and each peer that accepted
/// nothing for the stall limit is cut to free its share.
#[derive(Debug, Clone)]
pub(crate) struct ForwardBudget {
    bytes: Arc<Semaphore>,
    /// Pipes waiting for budget right now.
    starved: Arc<AtomicUsize>,
}

impl Default for ForwardBudget {
    fn default() -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(SESSION_BUFFER_BUDGET)),
            starved: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ForwardBudget {
    /// Reserves room for a chunk of `len` bytes, waiting while the budget is
    /// exhausted. `None` once the budget is gone (never closed in practice).
    async fn reserve(&self, len: usize) -> Option<OwnedSemaphorePermit> {
        let charge = len.clamp(MIN_CHUNK_CHARGE, SESSION_BUFFER_BUDGET);
        let charge = u32::try_from(charge).unwrap_or(u32::MAX);
        if let Ok(permit) = Arc::clone(&self.bytes).try_acquire_many_owned(charge) {
            return Some(permit);
        }
        let _starving = Starving::new(&self.starved);
        Arc::clone(&self.bytes)
            .acquire_many_owned(charge)
            .await
            .ok()
    }

    /// True while some pipe waits for budget (so russh's loop is waiting).
    fn exhausted(&self) -> bool {
        self.starved.load(Ordering::Relaxed) > 0
    }
}

/// Counts a pipe as starved for as long as it lives (cancellation included).
struct Starving<'a>(&'a AtomicUsize);

impl<'a> Starving<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for Starving<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Writes `chunk` to the local peer. A peer that accepts nothing for
/// `stall` is cut only if the session's budget is exhausted meanwhile:
/// otherwise it is merely paused, and its data waits in the budget.
async fn write_or_stall(
    peer: &mut tokio::net::tcp::OwnedWriteHalf,
    mut chunk: &[u8],
    stall: Duration,
    budget: &ForwardBudget,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    while !chunk.is_empty() {
        // `write` on a TCP stream is cancel-safe: a timed-out attempt wrote
        // nothing.
        match tokio::time::timeout(stall, peer.write(chunk)).await {
            Ok(Ok(0)) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(Ok(n)) => chunk = chunk.get(n..).unwrap_or_default(),
            Ok(Err(e)) => return Err(e),
            Err(_) if budget.exhausted() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the local peer stopped reading",
                ));
            }
            Err(_) => {}
        }
    }
    Ok(())
}

/// Copies both ways until both sides are done, the tunnel stops, or the
/// local peer stalls. SSH → peer data is read at once and queued within the
/// session's [`ForwardBudget`], drained by its own task, so russh's session
/// loop never waits on a slow peer for more than the session's
/// [`stall_limit`] (and only once the budget is exhausted).
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
    // Bounded in bytes by the budget; the chunk count never limits first.
    let (queue, mut queued) =
        tokio::sync::mpsc::channel::<(bytes::Bytes, OwnedSemaphorePermit)>(MAX_QUEUED_CHUNKS);
    let stall = ctx.stall_limit();
    let budget = ctx.forward_budget.clone();
    let reserve = ctx.forward_budget.clone();

    let from_ssh = async move {
        loop {
            let mut buf = bytes::BytesMut::with_capacity(PIPE_CHUNK);
            if ssh_read.read_buf(&mut buf).await? == 0 {
                return Ok::<_, std::io::Error>(());
            }
            let Some(permit) = reserve.reserve(buf.len()).await else {
                return Ok(());
            };
            if queue.send((buf.freeze(), permit)).await.is_err() {
                return Ok(());
            }
        }
    };
    let to_peer = async move {
        // The permit is released once its chunk is handed to the peer.
        while let Some((chunk, _permit)) = queued.recv().await {
            write_or_stall(&mut peer_write, &chunk, stall, &budget).await?;
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
        assert!(ForwardSpec::parse_cli("-L nope").is_err());
        assert!(ForwardSpec::parse_cli("-X 1080").is_err());
    }

    #[test]
    fn parses_dynamic_forwards() {
        let dynamic = |h: &str, p| ForwardSpec::Dynamic { bind: ep(h, p) };
        assert_eq!(
            ForwardSpec::parse_cli("-D 1080").unwrap(),
            dynamic("localhost", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_cli(" d 1080 ").unwrap(),
            dynamic("localhost", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_cli("-D 0.0.0.0:1080").unwrap(),
            dynamic("0.0.0.0", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_cli("-D [::1]:1080").unwrap(),
            dynamic("::1", 1080)
        );
        // `DynamicForward` values from ssh_config.
        assert_eq!(
            ForwardSpec::parse_dynamic("127.0.0.1:9050").unwrap(),
            dynamic("127.0.0.1", 9050)
        );
        assert_eq!(
            ForwardSpec::parse_dynamic("*:1080").unwrap(),
            dynamic("0.0.0.0", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_dynamic(":1080").unwrap(),
            dynamic("0.0.0.0", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_dynamic("0").unwrap(),
            dynamic("localhost", 0),
            "ephemeral port"
        );
        // OpenSSH's `bind/port` form (IPv6 needs no brackets there).
        assert_eq!(
            ForwardSpec::parse_dynamic("127.0.0.1/1080").unwrap(),
            dynamic("127.0.0.1", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_cli("-D ::1/1080").unwrap(),
            dynamic("::1", 1080)
        );
        assert_eq!(
            ForwardSpec::parse_dynamic("localhost/9050").unwrap(),
            dynamic("localhost", 9050)
        );
        assert_eq!(
            ForwardSpec::parse_dynamic("*/1080").unwrap(),
            dynamic("0.0.0.0", 1080)
        );
        for bad in [
            "127.0.0.1/",
            "127.0.0.1/1080/2",
            "[::1]/1080",
            "a b/1080",
            "host/port",
        ] {
            assert!(ForwardSpec::parse_dynamic(bad).is_err(), "{bad:?}");
        }
        for bad in [
            "",
            "-D",
            "-D ",
            "-D nope",
            "-D 70000",
            "-D -1",
            "-D ::1:1080",
            "-D a:b:1080",
            "-D 1080:db:22",
            "-D a b:1080",
        ] {
            assert!(ForwardSpec::parse_cli(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn dynamic_display_exposure_and_serde() {
        let socks = ForwardSpec::parse_cli("-D 1080").unwrap();
        assert_eq!(socks.to_string(), "SOCKS · localhost:1080");
        assert!(!socks.is_exposed());
        assert_eq!(socks.bind(), &ep("localhost", 1080));
        let exposed = ForwardSpec::parse_cli("-D 0.0.0.0:1080").unwrap();
        assert!(exposed.is_exposed());
        assert_eq!(
            ForwardSpec::parse_cli("-D [::1]:1080").unwrap().to_string(),
            "SOCKS · [::1]:1080"
        );
        let json = serde_json::to_string(&socks).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"dynamic","bind":{"host":"localhost","port":1080}}"#
        );
    }

    #[test]
    fn socks_failures_are_logged_readably() {
        assert_eq!(
            shown_target(&ep("2001:db8::1", 443)).to_string(),
            "[2001:db8::1]:443"
        );
        let hostile = shown_target(&ep("db\u{202e}.lan", 80)).to_string();
        assert!(!hostile.contains('\u{202e}') && hostile.ends_with(".lan:80"));
        let connect_failed = russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed);
        assert_eq!(
            refusal_reason(&ep("db.internal", 5432), &connect_failed),
            "refused or unresolvable on the server"
        );
        assert_eq!(
            refusal_reason(&ep("::1", 5432), &connect_failed),
            "refused or unreachable from the server"
        );
    }

    #[tokio::test]
    async fn log_throttle_counts_beyond_its_quota() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::Engine::new(crate::EngineConfig {
            paths: crate::store::Paths::rooted(dir.path()),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: Arc::new(crate::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: crate::engine::DEFAULT_CONNECT_TIMEOUT,
            keepalive: crate::engine::DEFAULT_KEEPALIVE,
            update_fetcher: crate::update::no_network(),
        });
        let ring = Arc::new(Mutex::new(crate::logs::LogRing::new(16)));
        let ctx = SessionCtx::new(
            engine,
            SessionId::next(),
            CancellationToken::new(),
            Arc::clone(&ring),
        );
        let throttle = Throttle::new(2, "dropped");
        let mut built = 0;
        for _ in 0..10 {
            throttle.warn(&ctx, || {
                built += 1;
                "line".to_owned()
            });
        }
        assert_eq!(built, 2, "messages beyond the quota are not even built");
        throttle.flush(&ctx);
        let messages: Vec<String> = ring
            .lock()
            .unwrap()
            .iter()
            .map(|l| l.message.clone())
            .collect();
        assert_eq!(messages, ["line", "line", "8 dropped"]);
    }

    #[tokio::test]
    async fn budget_reports_starvation_while_waiting() {
        let budget = ForwardBudget::default();
        let all = budget.reserve(SESSION_BUFFER_BUDGET).await.unwrap();
        assert!(!budget.exhausted());
        let waiting = budget.reserve(1);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        assert!(budget.exhausted(), "a pipe waits for budget");
        drop(all);
        assert!(waiting.await.is_some());
        assert!(!budget.exhausted());
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
