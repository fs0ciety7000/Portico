//! Interactive PTY channels (one per terminal tab).
//!
//! Data plane: PTY output flows to the UI through a bounded `mpsc` of
//! [`Bytes`]; when the UI is slow, `send().await` stalls this task, russh
//! stops granting window to the server, and the server stops sending —
//! end-to-end backpressure without unbounded buffers. Keystrokes and
//! resizes come back through a second bounded channel.

use bytes::Bytes;
use russh::client::{self, Msg};
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty};
use tokio::sync::mpsc;

use super::SessionCtx;
use super::handler::ClientHandler;
use crate::engine::{PtySize, TERMINAL_OUTPUT_CAPACITY, TerminalHandle, TerminalInput};
use crate::error::{Error, Result};
use crate::events::{EngineEvent, TerminalEvent};
use crate::ids::TerminalId;
use crate::logs::LogLevel;

/// `TERM` announced to the server (what both UI emulators implement).
pub(crate) const TERM: &str = "xterm-256color";
/// Pending keystroke / resize messages per terminal.
const INPUT_CAPACITY: usize = 64;
/// Modes sent with the PTY request: DEL as erase, UTF-8 input.
const MODES: &[(Pty, u32)] = &[(Pty::VERASE, 0x7f), (Pty::IUTF8, 1)];

/// Opens a session channel with a PTY and a login shell.
pub(crate) async fn open(
    ctx: &SessionCtx,
    handle: &client::Handle<ClientHandler>,
    size: PtySize,
) -> Result<TerminalHandle> {
    let target = || {
        ctx.engine
            .session_alias(ctx.id)
            .unwrap_or_else(|| ctx.id.to_string())
    };
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| Error::Transport {
            target: target(),
            message: format!("cannot open a session channel: {e}"),
        })?;
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
        .map_err(|e| transport(&target(), &e))?;
    expect_success(&mut channel, "pseudo-terminal").await?;
    channel
        .request_shell(true)
        .await
        .map_err(|e| transport(&target(), &e))?;
    expect_success(&mut channel, "shell").await?;

    let id = TerminalId::next();
    let (input_tx, input_rx) = mpsc::channel(INPUT_CAPACITY);
    let (output_tx, output_rx) = mpsc::channel(TERMINAL_OUTPUT_CAPACITY);
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
    tokio::spawn(pump(ctx.clone(), id, read, write, input_rx, output_tx));
    Ok(TerminalHandle {
        id,
        session: ctx.id,
        input: input_tx,
        output: output_rx,
    })
}

fn transport(target: &str, e: &russh::Error) -> Error {
    Error::Transport {
        target: target.to_owned(),
        message: e.to_string(),
    }
}

/// Waits for the reply to a `want_reply` channel request.
async fn expect_success(channel: &mut russh::Channel<Msg>, what: &'static str) -> Result<()> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => return Err(Error::TerminalRefused { what }),
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(Error::TerminalRefused { what });
            }
            Some(_) => {} // window adjustments etc.
        }
    }
}

async fn pump(
    ctx: SessionCtx,
    id: TerminalId,
    mut read: ChannelReadHalf,
    write: ChannelWriteHalf<Msg>,
    mut input: mpsc::Receiver<TerminalInput>,
    output: mpsc::Sender<Bytes>,
) {
    let emit = |event| {
        ctx.engine.emit(EngineEvent::Terminal {
            id,
            session: ctx.id,
            event,
        });
    };
    loop {
        tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => break,
            msg = read.wait() => match msg {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    if output.send(data).await.is_err() {
                        break; // the view went away
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    emit(TerminalEvent::Exited { status: Some(exit_status) });
                }
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    ctx.log(LogLevel::Info, "pty", format!("terminal {id}: remote process killed by {signal_name:?}"));
                    emit(TerminalEvent::Exited { status: None });
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            msg = input.recv() => match msg {
                Some(TerminalInput::Data(bytes)) => {
                    if write.data_bytes(bytes).await.is_err() {
                        break;
                    }
                }
                Some(TerminalInput::Resize(size)) => {
                    let _ = write
                        .window_change(
                            u32::from(size.cols),
                            u32::from(size.rows),
                            u32::from(size.pixel_width),
                            u32::from(size.pixel_height),
                        )
                        .await;
                }
                Some(TerminalInput::Close) | None => {
                    let _ = write.eof().await;
                    let _ = write.close().await;
                    break;
                }
            },
        }
    }
    ctx.log(LogLevel::Info, "pty", format!("terminal {id} closed"));
    emit(TerminalEvent::Closed);
}
