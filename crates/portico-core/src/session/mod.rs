//! SSH sessions: one actor task per connection.
//!
//! ```text
//! run ─▶ establish ─┬─ route::plan          (ssh_config, ProxyJump chain)
//!                   ├─ per hop: tcp / direct-tcpip ─▶ handshake (handler: known_hosts)
//!                   │                              └▶ auth::authenticate
//!                   └─ Connection ─▶ supervise (keepalive RTT, commands) ─▶ close
//! ```
//!
//! Every transition is published as a [`SessionPhase`] *before* the next step
//! starts, so UIs mirror the real state. Every noteworthy step is logged to
//! the session's [`LogRing`] (debug panel) — never with secret material.

mod auth;
mod handler;
mod route;
mod signer;
mod stream;
mod terminal;

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use russh::client;
use time::OffsetDateTime;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::engine::{ConnectRequest, DEFAULT_KEEPALIVE_MAX, Engine, PromptResponse};
use crate::error::{Error, Result};
use crate::events::{CloseReason, EngineEvent, PromptKind, SessionEvent, SessionPhase};
use crate::ids::SessionId;
use crate::logs::{LogLevel, LogRecord, LogRing};
use crate::ssh_config::SshConfig;

use handler::ClientHandler;
use route::Hop;
use stream::WatchedStream;

/// Delay before the first latency probe after login.
const FIRST_PROBE_DELAY: Duration = Duration::from_millis(500);
/// Network time allowed for authentication (time spent in prompts excluded).
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);
/// Opening a PTY + shell (channel open, pty-req, shell).
const TERMINAL_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long `close` waits for each hop's DISCONNECT to go out.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Commands accepted by a running session actor.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    Disconnect,
    OpenTerminal {
        size: crate::engine::PtySize,
        reply: tokio::sync::oneshot::Sender<Result<crate::engine::TerminalHandle>>,
    },
}

/// Everything a session task needs to report progress and ask questions.
#[derive(Debug, Clone)]
pub(crate) struct SessionCtx {
    pub(crate) engine: Engine,
    pub(crate) id: SessionId,
    pub(crate) cancel: CancellationToken,
    logs: Arc<Mutex<LogRing>>,
    /// Set while the user is answering a prompt (pauses network timeouts).
    prompting: Arc<AtomicBool>,
    /// When the last prompt was answered (grants a fresh timeout window).
    last_prompt_end: Arc<Mutex<Option<tokio::time::Instant>>>,
    /// Cancelled when the session ends: fails every transport stream so no
    /// russh task or socket outlives the session.
    abort: CancellationToken,
}

impl SessionCtx {
    pub(crate) fn new(
        engine: Engine,
        id: SessionId,
        cancel: CancellationToken,
        logs: Arc<Mutex<LogRing>>,
    ) -> Self {
        Self {
            engine,
            id,
            abort: cancel.child_token(),
            cancel,
            logs,
            prompting: Arc::new(AtomicBool::new(false)),
            last_prompt_end: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn event(&self, event: SessionEvent) {
        self.engine
            .emit(EngineEvent::Session { id: self.id, event });
    }

    pub(crate) fn phase(&self, phase: SessionPhase) {
        self.event(SessionEvent::Phase(phase));
    }

    /// Records a log line (ring buffer + event + tracing). Never pass secrets.
    pub(crate) fn log(&self, level: LogLevel, target: &str, message: impl Into<String>) {
        let record = LogRecord {
            at: OffsetDateTime::now_utc(),
            level,
            session: Some(self.id),
            target: target.to_owned(),
            message: message.into(),
        };
        tracing::debug!(session = %self.id, target, "{}", record.message);
        self.logs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record.clone());
        self.engine.emit(EngineEvent::Log(record));
    }

    pub(crate) async fn ask(&self, kind: PromptKind) -> Result<PromptResponse> {
        self.prompting.store(true, Ordering::SeqCst);
        let answer = self.engine.ask(self.id, kind, &self.cancel).await;
        *self
            .last_prompt_end
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tokio::time::Instant::now());
        self.prompting.store(false, Ordering::SeqCst);
        answer
    }

    /// Runs `fut` with a network deadline that is suspended while the user
    /// answers a prompt and restarts in full once they did.
    async fn with_deadline<T>(
        &self,
        limit: Duration,
        phase: &'static str,
        target: &str,
        fut: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        tokio::pin!(fut);
        let mut deadline = tokio::time::Instant::now() + limit;
        loop {
            tokio::select! {
                result = &mut fut => return result,
                () = tokio::time::sleep_until(deadline) => {
                    let now = tokio::time::Instant::now();
                    if self.prompting.load(Ordering::SeqCst) {
                        deadline = now + limit;
                        continue;
                    }
                    let answered = *self.last_prompt_end.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(end) = answered.filter(|end| *end + limit > now) {
                        deadline = end + limit;
                        continue;
                    }
                    return Err(Error::Timeout { phase, target: target.to_owned() });
                }
            }
        }
    }
}

/// Session actor entry point (spawned by [`Engine::connect`]).
pub(crate) async fn run(
    ctx: SessionCtx,
    request: ConnectRequest,
    mut commands: mpsc::Receiver<SessionCommand>,
) {
    ctx.event(SessionEvent::Started {
        alias: request.alias.clone(),
    });
    let established = tokio::select! {
        biased;
        () = ctx.cancel.cancelled() => Err(Error::Cancelled),
        r = establish(&ctx, &request) => r,
    };
    let conn = match established {
        Ok(conn) => conn,
        Err(e) => {
            // Kill half-open transports (russh tasks, jump channels, sockets).
            ctx.abort.cancel();
            if matches!(e, Error::Cancelled) {
                ctx.log(LogLevel::Info, "session", "connection aborted by the user");
                ctx.phase(SessionPhase::Closed {
                    reason: CloseReason::UserRequested,
                });
            } else {
                ctx.log(LogLevel::Error, "session", e.to_string());
                ctx.phase(SessionPhase::Failed { error: e.summary() });
            }
            return;
        }
    };

    ctx.phase(SessionPhase::Connected);
    let reason = conn.supervise(&ctx, &mut commands).await;
    ctx.phase(SessionPhase::Disconnecting);
    conn.close().await;
    ctx.abort.cancel();
    ctx.log(LogLevel::Info, "session", format!("closed ({reason:?})"));
    ctx.phase(SessionPhase::Closed { reason });
}

/// One authenticated transport and its "closed by peer" signal.
struct Link {
    /// Shared so channel openings can run in their own tasks.
    handle: Arc<client::Handle<ClientHandler>>,
    closed: CancellationToken,
}

/// An authenticated chain of transports; the last one is the target.
struct Connection {
    links: Vec<Link>,
    keepalive: Duration,
    keepalive_max: u32,
    /// `ServerAliveInterval 0`: probe for latency, never disconnect.
    disconnect_on_silence: bool,
}

async fn establish(ctx: &SessionCtx, request: &ConnectRequest) -> Result<Connection> {
    ctx.phase(SessionPhase::Resolving);
    let config = ctx.engine.config().clone();
    let alias = request.alias.clone();
    let user = request.user_override.clone();
    let hops = tokio::task::spawn_blocking(move || {
        let ssh = SshConfig::load(&config.paths.ssh_config(), &config.paths.ssh_dir)?;
        route::plan(&ssh, &config, &alias, user)
    })
    .await
    .map_err(|_| Error::EngineClosed)??;

    let route: Vec<String> = hops.iter().map(Hop::describe).collect();
    ctx.log(LogLevel::Info, "route", route.join(" → "));

    let total = u8::try_from(hops.len()).unwrap_or(u8::MAX);
    let mut links: Vec<Link> = Vec::with_capacity(hops.len());
    for (index, hop) in hops.iter().enumerate() {
        ctx.phase(SessionPhase::Connecting {
            hop: u8::try_from(index + 1).unwrap_or(u8::MAX),
            hops: total,
            target: hop.alias.clone(),
        });
        let limit = hop.connect_timeout(ctx.engine.config().connect_timeout);
        let built = handler::build(ctx, hop).await?;
        let closed = built.closed.clone();
        let identity_slot = Arc::clone(&built.identity);
        let mut handle = open_transport(ctx, hop, built, links.last(), limit, &closed).await?;
        let identity = identity_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| Error::Transport {
                target: hop.alias.clone(),
                message: "server key was not verified".to_owned(),
            })?;
        let method = ctx
            .with_deadline(
                AUTH_TIMEOUT,
                "authenticating to",
                &hop.alias,
                auth::authenticate(ctx, &mut handle, hop, &identity),
            )
            .await?;
        ctx.log(
            LogLevel::Info,
            "auth",
            format!(
                "authenticated to {} as {} ({method:?})",
                hop.alias, hop.user
            ),
        );
        links.push(Link {
            handle: Arc::new(handle),
            closed,
        });
    }

    let target = hops.last();
    let interval = target.and_then(|h| h.option_u32("serveraliveinterval"));
    Ok(Connection {
        links,
        keepalive: interval
            .filter(|s| *s > 0)
            .map_or(ctx.engine.config().keepalive, |s| {
                Duration::from_secs(u64::from(s))
            }),
        keepalive_max: target
            .and_then(|h| h.option_u32("serveralivecountmax"))
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_KEEPALIVE_MAX),
        disconnect_on_silence: interval != Some(0),
    })
}

/// Opens the transport to `hop` (TCP, or a direct-tcpip channel through the
/// previous hop) and runs the SSH handshake on it.
async fn open_transport(
    ctx: &SessionCtx,
    hop: &Hop,
    built: handler::Built,
    previous: Option<&Link>,
    limit: Duration,
    closed: &CancellationToken,
) -> Result<client::Handle<ClientHandler>> {
    let Some(previous) = previous else {
        let stream = tcp_connect(ctx, hop, limit).await?;
        let stream = WatchedStream::new(stream, closed.clone(), ctx.abort.clone());
        return handshake(ctx, hop, built, stream, limit).await;
    };
    ctx.log(
        LogLevel::Info,
        "jump",
        format!(
            "opening direct-tcpip to {}:{}",
            hop.resolved.hostname, hop.resolved.port
        ),
    );
    let open = async {
        previous
            .handle
            .channel_open_direct_tcpip(
                hop.resolved.hostname.clone(),
                u32::from(hop.resolved.port),
                "127.0.0.1",
                0,
            )
            .await
            .map_err(|e| transport(hop, &e))
    };
    let channel = ctx
        .with_deadline(limit, "opening a jump channel to", &hop.alias, open)
        .await?;
    let stream = WatchedStream::new(channel.into_stream(), closed.clone(), ctx.abort.clone());
    handshake(ctx, hop, built, stream, limit).await
}

async fn tcp_connect(ctx: &SessionCtx, hop: &Hop, limit: Duration) -> Result<TcpStream> {
    let host = hop.resolved.hostname.clone();
    let port = hop.resolved.port;
    let addrs: Vec<std::net::SocketAddr> =
        tokio::time::timeout(limit, tokio::net::lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| Error::Timeout {
                phase: "resolving",
                target: host.clone(),
            })?
            .map_err(|source| Error::Resolve {
                host: host.clone(),
                source,
            })?
            .collect();

    let mut last_error = std::io::Error::new(std::io::ErrorKind::NotFound, "no address");
    for addr in addrs {
        ctx.log(LogLevel::Debug, "tcp", format!("connecting to {addr}"));
        match tokio::time::timeout(limit, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                // Interactive traffic: latency matters more than packet count.
                let _ = stream.set_nodelay(true);
                ctx.log(LogLevel::Info, "tcp", format!("connected to {addr}"));
                return Ok(stream);
            }
            Ok(Err(e)) => {
                ctx.log(LogLevel::Warn, "tcp", format!("{addr}: {e}"));
                last_error = e;
            }
            Err(_) => {
                ctx.log(LogLevel::Warn, "tcp", format!("{addr}: timed out"));
                last_error = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
            }
        }
    }
    if last_error.kind() == std::io::ErrorKind::TimedOut {
        return Err(Error::Timeout {
            phase: "connecting to",
            target: format!("{host}:{port}"),
        });
    }
    Err(Error::Connect {
        target: format!("{host}:{port}"),
        source: last_error,
    })
}

/// Runs the SSH handshake (key exchange + host key verification).
async fn handshake<S>(
    ctx: &SessionCtx,
    hop: &Hop,
    built: handler::Built,
    stream: S,
    limit: Duration,
) -> Result<client::Handle<ClientHandler>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    ctx.phase(SessionPhase::Handshake);
    let handler::Built {
        handler,
        config,
        failure,
        ..
    } = built;
    let negotiate = async {
        client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| {
                let recorded = failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                recorded.map_or_else(|| transport(hop, &e), Error::HostKey)
            })
    };
    ctx.with_deadline(limit, "negotiating with", &hop.alias, negotiate)
        .await
}

async fn open_terminal(
    ctx: SessionCtx,
    handle: Arc<client::Handle<ClientHandler>>,
    size: crate::engine::PtySize,
    reply: tokio::sync::oneshot::Sender<Result<crate::engine::TerminalHandle>>,
) {
    let opened = tokio::select! {
        biased;
        () = ctx.cancel.cancelled() => Err(Error::SessionEnded(ctx.id)),
        opened = terminal::open(&ctx, &handle, size, TERMINAL_OPEN_TIMEOUT) => opened,
    };
    if let Err(e) = &opened {
        ctx.log(LogLevel::Error, "pty", e.to_string());
    }
    let _ = reply.send(opened);
}

pub(crate) fn transport(hop: &Hop, e: &russh::Error) -> Error {
    Error::Transport {
        target: hop.alias.clone(),
        message: e.to_string(),
    }
}

type Probe<'a> = std::pin::Pin<Box<dyn Future<Output = Option<Duration>> + Send + 'a>>;

impl Connection {
    async fn supervise(
        &self,
        ctx: &SessionCtx,
        commands: &mut mpsc::Receiver<SessionCommand>,
    ) -> CloseReason {
        let Some(target) = self.links.last() else {
            return CloseReason::RemoteClosed;
        };
        // First probe slightly after login: right after authentication the
        // peer may delay its ACKs, which would inflate the first RTT sample.
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + FIRST_PROBE_DELAY.min(self.keepalive),
            self.keepalive,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut missed = 0u32;
        // The probe runs alongside the select so Disconnect / cancel / remote
        // close are never delayed by a ping stuck on a dead network.
        let mut probe: Option<Probe<'_>> = None;
        loop {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return CloseReason::UserRequested,
                () = target.closed.cancelled() => {
                    ctx.log(LogLevel::Warn, "session", "connection closed by the remote host");
                    return CloseReason::RemoteClosed;
                }
                cmd = commands.recv() => match cmd {
                    Some(SessionCommand::Disconnect) | None => return CloseReason::UserRequested,
                    Some(SessionCommand::OpenTerminal { size, reply }) => {
                        // Off the supervisor loop: keepalive, Disconnect and
                        // remote close stay responsive while a shell starts.
                        tokio::spawn(open_terminal(ctx.clone(), Arc::clone(&target.handle), size, reply));
                    }
                },
                Some(rtt) = futures::future::OptionFuture::from(probe.as_mut()) => {
                    probe = None;
                    if let Some(rtt) = rtt {
                        missed = 0;
                        ctx.event(SessionEvent::Latency { rtt });
                    } else {
                        missed += 1;
                        ctx.log(
                            LogLevel::Warn,
                            "keepalive",
                            format!("no keepalive reply ({missed}/{})", self.keepalive_max),
                        );
                        if target.handle.is_closed() {
                            return CloseReason::RemoteClosed;
                        }
                        if self.disconnect_on_silence && missed >= self.keepalive_max {
                            return CloseReason::Timeout;
                        }
                    }
                }
                _ = ticker.tick(), if probe.is_none() => {
                    let (handle, limit) = (&target.handle, self.keepalive);
                    probe = Some(Box::pin(async move {
                        let started = Instant::now();
                        match tokio::time::timeout(limit, handle.send_ping()).await {
                            Ok(Ok(())) if !handle.is_closed() => Some(started.elapsed()),
                            _ => None,
                        }
                    }));
                }
            }
        }
    }

    /// Disconnects from the target back to the first hop, letting each
    /// DISCONNECT travel through the still-open jump channels.
    async fn close(&self) {
        for link in self.links.iter().rev() {
            let _ = link
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
            let _ = tokio::time::timeout(CLOSE_GRACE, link.closed.cancelled()).await;
        }
    }
}
