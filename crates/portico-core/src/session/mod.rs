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

/// Delay before the first latency probe after login.
const FIRST_PROBE_DELAY: Duration = Duration::from_millis(500);

/// Commands accepted by a running session actor.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    Disconnect,
}

/// Everything a session task needs to report progress and ask questions.
#[derive(Debug, Clone)]
pub(crate) struct SessionCtx {
    pub(crate) engine: Engine,
    pub(crate) id: SessionId,
    pub(crate) cancel: CancellationToken,
    logs: Arc<Mutex<LogRing>>,
    /// Set while the user is answering a prompt (pauses handshake timeouts).
    prompting: Arc<AtomicBool>,
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
            cancel,
            logs,
            prompting: Arc::new(AtomicBool::new(false)),
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
        self.prompting.store(false, Ordering::SeqCst);
        answer
    }

    fn is_prompting(&self) -> bool {
        self.prompting.load(Ordering::SeqCst)
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
        Err(Error::Cancelled) => {
            ctx.log(LogLevel::Info, "session", "connection aborted by the user");
            ctx.phase(SessionPhase::Closed {
                reason: CloseReason::UserRequested,
            });
            return;
        }
        Err(e) => {
            ctx.log(LogLevel::Error, "session", e.to_string());
            ctx.phase(SessionPhase::Failed { error: e.summary() });
            return;
        }
    };

    ctx.phase(SessionPhase::Connected);
    let reason = conn.supervise(&ctx, &mut commands).await;
    ctx.phase(SessionPhase::Disconnecting);
    conn.close().await;
    ctx.log(LogLevel::Info, "session", format!("closed ({reason:?})"));
    ctx.phase(SessionPhase::Closed { reason });
}

/// An authenticated chain of transports; the last one is the target.
struct Connection {
    hops: Vec<client::Handle<ClientHandler>>,
    target_closed: CancellationToken,
    keepalive: Duration,
    keepalive_max: u32,
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
    let mut handles: Vec<client::Handle<ClientHandler>> = Vec::with_capacity(hops.len());
    let mut target_closed = CancellationToken::new();
    for (index, hop) in hops.iter().enumerate() {
        ctx.phase(SessionPhase::Connecting {
            hop: u8::try_from(index + 1).unwrap_or(u8::MAX),
            hops: total,
            target: hop.alias.clone(),
        });
        let limit = hop.connect_timeout(ctx.engine.config().connect_timeout);
        let built = handler::build(ctx, hop).await?;
        let closed = built.closed.clone();
        let mut handle = match handles.last() {
            None => {
                let stream = tcp_connect(ctx, hop, limit).await?;
                handshake(ctx, hop, built, stream, limit).await?
            }
            Some(previous) => {
                ctx.log(
                    LogLevel::Info,
                    "jump",
                    format!(
                        "opening direct-tcpip to {}:{}",
                        hop.resolved.hostname, hop.resolved.port
                    ),
                );
                let channel = tokio::time::timeout(
                    limit,
                    previous.channel_open_direct_tcpip(
                        hop.resolved.hostname.clone(),
                        u32::from(hop.resolved.port),
                        "127.0.0.1",
                        0,
                    ),
                )
                .await
                .map_err(|_| Error::Timeout {
                    phase: "opening a jump channel to",
                    target: hop.alias.clone(),
                })?
                .map_err(|e| transport(hop, &e))?;
                handshake(ctx, hop, built, channel.into_stream(), limit).await?
            }
        };
        let method = auth::authenticate(ctx, &mut handle, hop).await?;
        ctx.log(
            LogLevel::Info,
            "auth",
            format!(
                "authenticated to {} as {} ({method:?})",
                hop.alias, hop.user
            ),
        );
        target_closed = closed;
        handles.push(handle);
    }

    let target = hops.last();
    Ok(Connection {
        hops: handles,
        target_closed,
        keepalive: target
            .and_then(|h| h.resolved.server_alive_interval)
            .filter(|s| *s > 0)
            .map_or(ctx.engine.config().keepalive, |s| {
                Duration::from_secs(u64::from(s))
            }),
        keepalive_max: target
            .and_then(|h| h.option_u32("serveralivecountmax"))
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_KEEPALIVE_MAX),
    })
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

/// Runs the SSH handshake. The timeout is paused while the user answers a
/// host key prompt.
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
    let fut = client::connect_stream(config, stream, handler);
    tokio::pin!(fut);
    let mut deadline = tokio::time::Instant::now() + limit;
    loop {
        tokio::select! {
            result = &mut fut => {
                return result.map_err(|e| {
                    let recorded = failure.lock().unwrap_or_else(PoisonError::into_inner).take();
                    recorded.map_or_else(|| transport(hop, &e), Error::HostKey)
                });
            }
            () = tokio::time::sleep_until(deadline) => {
                if ctx.is_prompting() {
                    deadline = tokio::time::Instant::now() + limit;
                    continue;
                }
                return Err(Error::Timeout { phase: "negotiating with", target: hop.alias.clone() });
            }
        }
    }
}

pub(crate) fn transport(hop: &Hop, e: &russh::Error) -> Error {
    Error::Transport {
        target: hop.alias.clone(),
        message: e.to_string(),
    }
}

impl Connection {
    async fn supervise(
        &self,
        ctx: &SessionCtx,
        commands: &mut mpsc::Receiver<SessionCommand>,
    ) -> CloseReason {
        let Some(target) = self.hops.last() else {
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
        loop {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return CloseReason::UserRequested,
                () = self.target_closed.cancelled() => {
                    ctx.log(LogLevel::Warn, "session", "connection closed by the remote host");
                    return CloseReason::RemoteClosed;
                }
                cmd = commands.recv() => match cmd {
                    Some(SessionCommand::Disconnect) | None => return CloseReason::UserRequested,
                },
                _ = ticker.tick() => {
                    let started = Instant::now();
                    let probe = tokio::time::timeout(self.keepalive, target.send_ping()).await;
                    match probe {
                        Ok(Ok(())) if !target.is_closed() => {
                            missed = 0;
                            ctx.event(SessionEvent::Latency { rtt: started.elapsed() });
                        }
                        _ => {
                            missed += 1;
                            ctx.log(LogLevel::Warn, "keepalive", format!("no keepalive reply ({missed}/{})", self.keepalive_max));
                            if target.is_closed() {
                                return CloseReason::RemoteClosed;
                            }
                            if missed >= self.keepalive_max {
                                return CloseReason::Timeout;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn close(&self) {
        for handle in self.hops.iter().rev() {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }
    }
}
