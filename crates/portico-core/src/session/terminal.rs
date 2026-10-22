//! Interactive PTY channels (one per terminal tab).
//!
//! Each terminal runs **two independent tasks** over the split channel:
//! - the *reader* forwards PTY output to the UI through a bounded `mpsc`;
//! - the *writer* sends keystrokes, resizes and the final EOF + CLOSE.
//!
//! They never wait on each other: Ctrl-C is delivered during an output
//! flood, and a writer stalled on the remote window can never stop the
//! reader (which would freeze russh's session loop and every other channel).
//!
//! Recording (phase 6): a [`TerminalTap`] sits on the output path of the
//! reader, after a chunk was handed to the UI, so a recording holds exactly
//! what the UI received; the writer reports resizes to it. Neither ever
//! waits on the recording (`try_send`, see [`crate::recording`]). When the
//! terminal ends, the tap is closed, which stops a running recording.
//!
//! Backpressure: russh 0.64 re-opens the SSH window as soon as a packet is
//! received, so a slow UI is throttled at the TCP level for the *whole
//! connection* (russh's per-channel queue, then the session loop, then the
//! socket). Memory stays bounded either way. Per-channel flow control would
//! need russh to let the handler time window adjustments (upstream limit).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use russh::client::{self, Msg};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::SessionCtx;
use super::handler::ClientHandler;
use crate::engine::{
    PtySize, TERMINAL_INPUT_CAPACITY, TERMINAL_OUTPUT_CAPACITY, TerminalHandle, TerminalInputs,
};
use crate::error::{Error, Result};
use crate::events::{EngineEvent, TerminalEvent};
use crate::ids::TerminalId;
use crate::logs::LogLevel;
use crate::recording::TerminalTap;

/// `TERM` announced to the server (what both UI emulators implement).
pub(crate) const TERM: &str = "xterm-256color";
/// Terminal modes: DEL erases, CR→NL on input (Enter sends CR), UTF-8,
/// nominal line speed.
const MODES: &[(Pty, u32)] = &[
    (Pty::VERASE, 0x7f),
    (Pty::ICRNL, 1),
    (Pty::IUTF8, 1),
    (Pty::TTY_OP_ISPEED, 38_400),
    (Pty::TTY_OP_OSPEED, 38_400),
];
/// Time allowed for the final EOF + CLOSE to be queued.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Opens a session channel with a PTY and a login shell.
pub(crate) async fn open(
    ctx: &SessionCtx,
    handle: &client::Handle<ClientHandler>,
    size: PtySize,
    limit: Duration,
) -> Result<TerminalHandle> {
    let target = ctx
        .engine
        .session_alias(ctx.id)
        .unwrap_or_else(|| ctx.id.to_string());
    let timeout = || Error::Timeout {
        phase: "opening a terminal on",
        target: target.clone(),
    };
    let mut channel = tokio::time::timeout(limit, handle.channel_open_session())
        .await
        .map_err(|_| timeout())?
        .map_err(|e| Error::Transport {
            target: target.clone(),
            message: format!("cannot open a session channel: {e}"),
        })?;
    let negotiated = tokio::time::timeout(limit, negotiate(ctx, &mut channel, size, &target))
        .await
        .unwrap_or_else(|_| Err(timeout()));
    let prelude = match negotiated {
        Ok(prelude) => prelude,
        Err(e) => {
            // Never leave a half-open channel (and maybe a shell) behind.
            let _ = tokio::time::timeout(CLOSE_GRACE, channel.close()).await;
            return Err(e);
        }
    };

    let id = TerminalId::next();
    let (data_tx, data_rx) = mpsc::channel(TERMINAL_INPUT_CAPACITY);
    let (size_tx, size_rx) = watch::channel(size);
    let (output_tx, output_rx) = mpsc::channel(TERMINAL_OUTPUT_CAPACITY);
    let close = ctx.cancel.child_token();
    let peer_closed = Arc::new(AtomicBool::new(false));
    let tap = Arc::new(TerminalTap::new(size));
    ctx.engine.register_terminal(id, ctx.id, &tap);
    for chunk in prelude {
        // Output that arrived before the shell was confirmed.
        let _ = output_tx.try_send(chunk);
    }
    let (read, write) = channel.split();
    ctx.log(
        LogLevel::Info,
        "pty",
        format!("terminal {id} opened ({}x{}, {TERM})", size.cols, size.rows),
    );
    ctx.engine.emit(EngineEvent::Terminal {
        id,
        session: ctx.id,
        event: TerminalEvent::Opened,
    });
    tokio::spawn(reader(
        ctx.clone(),
        id,
        read,
        output_tx,
        close.clone(),
        Arc::clone(&peer_closed),
        Arc::clone(&tap),
    ));
    tokio::spawn(writer(
        ctx.clone(),
        id,
        write,
        data_rx,
        size_rx,
        close.clone(),
        peer_closed,
        tap,
    ));
    Ok(TerminalHandle {
        id,
        session: ctx.id,
        input: TerminalInputs::new(data_tx, size_tx, close),
        output: output_rx,
    })
}

/// pty-req + shell, each with `want_reply`. Returns output received before
/// the shell was confirmed (some servers print a banner first).
async fn negotiate(
    ctx: &SessionCtx,
    channel: &mut russh::Channel<Msg>,
    size: PtySize,
    target: &str,
) -> Result<Vec<Bytes>> {
    let transport = |e: russh::Error| Error::Transport {
        target: target.to_owned(),
        message: e.to_string(),
    };
    channel
        .request_pty(
            true,
            TERM,
            u32::from(size.cols),
            u32::from(size.rows),
            u32::from(size.pixel_width),
            u32::from(size.pixel_height),
            MODES,
        )
        .await
        .map_err(transport)?;
    let mut prelude = Vec::new();
    expect_success(ctx, channel, "pseudo-terminal", &mut prelude).await?;
    channel.request_shell(true).await.map_err(transport)?;
    expect_success(ctx, channel, "shell", &mut prelude).await?;
    Ok(prelude)
}

/// Waits for the reply to a `want_reply` channel request.
async fn expect_success(
    ctx: &SessionCtx,
    channel: &mut russh::Channel<Msg>,
    what: &'static str,
    prelude: &mut Vec<Bytes>,
) -> Result<()> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => return Err(Error::TerminalRefused { what }),
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                prelude.push(data);
            }
            Some(ChannelMsg::Close | ChannelMsg::Eof) => {
                return Err(Error::TerminalRefused { what });
            }
            None => return Err(Error::SessionEnded(ctx.id)),
            Some(_) => {} // window adjustments etc.
        }
    }
}

/// Remote → UI. Only ever waits on the channel, the UI queue, or `close`.
async fn reader(
    ctx: SessionCtx,
    id: TerminalId,
    mut read: ChannelReadHalf,
    output: mpsc::Sender<Bytes>,
    close: CancellationToken,
    peer_closed: Arc<AtomicBool>,
    tap: Arc<TerminalTap>,
) {
    let mut exited = false;
    loop {
        let msg = tokio::select! {
            biased;
            () = close.cancelled() => break,
            msg = read.wait() => msg,
        };
        match msg {
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                // A refcount, not a copy.
                let recorded = data.clone();
                tokio::select! {
                    biased;
                    () = close.cancelled() => break,
                    sent = output.send(data) => {
                        if sent.is_err() {
                            // The view went away: close the channel.
                            close.cancel();
                            break;
                        }
                    }
                }
                // Exactly what the UI received; never waits.
                tap.output(&recorded);
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exited = true;
                emit(
                    &ctx,
                    id,
                    TerminalEvent::Exited {
                        status: Some(exit_status),
                        signal: None,
                    },
                );
            }
            Some(ChannelMsg::ExitSignal {
                signal_name,
                core_dumped,
                error_message,
                ..
            }) => {
                exited = true;
                let name = format!("{signal_name:?}");
                ctx.log(
                    LogLevel::Info,
                    "pty",
                    format!(
                        "terminal {id}: killed by SIG{name}{}{}",
                        if core_dumped { " (core dumped)" } else { "" },
                        if error_message.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", error_message.escape_debug())
                        },
                    ),
                );
                emit(
                    &ctx,
                    id,
                    TerminalEvent::Exited {
                        status: None,
                        signal: Some(name),
                    },
                );
            }
            // The process is gone and its output is complete: some servers
            // then wait for us to close.
            Some(ChannelMsg::Eof) if exited => {
                close.cancel();
                break;
            }
            Some(ChannelMsg::Close) | None => {
                // russh already answered the peer's CLOSE.
                peer_closed.store(true, Ordering::Release);
                close.cancel();
                break;
            }
            Some(_) => {}
        }
    }
}

/// UI → remote. Owns the channel's end of life: EOF + CLOSE on every exit
/// path, unless the peer closed first; then closes the recording tap.
// One argument per channel end, plus the tap.
#[allow(clippy::too_many_arguments)]
async fn writer(
    ctx: SessionCtx,
    id: TerminalId,
    write: ChannelWriteHalf<Msg>,
    mut data: mpsc::Receiver<Bytes>,
    mut size: watch::Receiver<PtySize>,
    close: CancellationToken,
    peer_closed: Arc<AtomicBool>,
    tap: Arc<TerminalTap>,
) {
    loop {
        tokio::select! {
            biased;
            () = close.cancelled() => break,
            changed = size.changed() => {
                if changed.is_err() {
                    break;
                }
                let s = *size.borrow_and_update();
                tap.resize(s);
                let _ = write
                    .window_change(
                        u32::from(s.cols),
                        u32::from(s.rows),
                        u32::from(s.pixel_width),
                        u32::from(s.pixel_height),
                    )
                    .await;
            }
            bytes = data.recv() => {
                let Some(bytes) = bytes else { break };
                // May wait for the remote window: the reader keeps running.
                let sent = tokio::select! {
                    biased;
                    () = close.cancelled() => break,
                    sent = write.data_bytes(bytes) => sent,
                };
                if sent.is_err() {
                    break;
                }
            }
        }
    }
    close.cancel();
    if !peer_closed.load(Ordering::Acquire) {
        let _ = tokio::time::timeout(CLOSE_GRACE, async {
            let _ = write.eof().await;
            let _ = write.close().await;
        })
        .await;
    }
    // Stops a running recording (`terminal_closed`): its writer flushes
    // and closes the file.
    ctx.engine.unregister_terminal(id);
    tap.close();
    ctx.log(LogLevel::Info, "pty", format!("terminal {id} closed"));
    emit(&ctx, id, TerminalEvent::Closed);
}

fn emit(ctx: &SessionCtx, id: TerminalId, event: TerminalEvent) {
    ctx.engine.emit(EngineEvent::Terminal {
        id,
        session: ctx.id,
        event,
    });
}
