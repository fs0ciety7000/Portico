//! Terminal data plane between `portico-core` and xterm.js.
//!
//! Output: one forwarder task per terminal drains [`TerminalHandle::output`],
//! coalesces what is already queued (one IPC message per burst, not per SSH
//! packet) and sends raw bytes on the view's `tauri::ipc::Channel`.
//!
//! Flow control: `Channel::send` never blocks, so without a limit a flood
//! would pile up in the webview. The view acknowledges bytes once xterm.js has
//! parsed them (`terminal_ack`); the forwarder stops reading while more than
//! [`HIGH_WATER`] bytes are unacknowledged, which lets the engine's bounded
//! queue push back to the SSH connection.
//!
//! Input, resize and close go through [`TerminalInputs`], looked up by id in
//! [`Terminals`]: the webview can only reach terminals it opened.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use portico_core::engine::{TerminalHandle, TerminalInputs};
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::watch;

/// Largest single IPC message (bytes).
const MAX_CHUNK: usize = 64 * 1024;
/// Unacknowledged bytes allowed in flight towards the webview.
pub(crate) const HIGH_WATER: u64 = 512 * 1024;

struct Entry {
    inputs: TerminalInputs,
    /// Total bytes the view has finished parsing.
    acked: watch::Sender<u64>,
}

/// Open terminals, by raw [`portico_core::ids::TerminalId`].
#[derive(Default)]
pub(crate) struct Terminals {
    map: Mutex<HashMap<u64, Entry>>,
}

impl Terminals {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Entry>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn inputs(&self, id: u64) -> Option<TerminalInputs> {
        self.lock().get(&id).map(|e| e.inputs.clone())
    }

    pub(crate) fn ack(&self, id: u64, total: u64) {
        if let Some(e) = self.lock().get(&id) {
            // Monotonic: a stale or bogus ack can never unlock more.
            e.acked.send_if_modified(|current| {
                let advance = total > *current;
                if advance {
                    *current = total;
                }
                advance
            });
        }
    }

    pub(crate) fn close(&self, id: u64) {
        if let Some(e) = self.lock().remove(&id) {
            e.inputs.close();
        }
    }

    /// Closes everything (window closing / app exit).
    pub(crate) fn close_all(&self) {
        for (_, e) in self.lock().drain() {
            e.inputs.close();
        }
    }

    /// Registers `handle` and starts forwarding its output to `channel`.
    pub(crate) fn attach(
        self: &Arc<Self>,
        handle: TerminalHandle,
        channel: Channel<InvokeResponseBody>,
    ) -> u64 {
        let id = handle.id.get();
        let (acked_tx, acked_rx) = watch::channel(0);
        self.lock().insert(
            id,
            Entry {
                inputs: handle.input.clone(),
                acked: acked_tx,
            },
        );
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            forward(handle, channel, acked_rx).await;
            this.close(id);
        });
        id
    }
}

async fn forward(
    mut handle: TerminalHandle,
    channel: Channel<InvokeResponseBody>,
    mut acked: watch::Receiver<u64>,
) {
    let mut sent: u64 = 0;
    loop {
        // Wait for the view to catch up before reading more.
        while sent.saturating_sub(*acked.borrow_and_update()) > HIGH_WATER {
            tokio::select! {
                changed = acked.changed() => if changed.is_err() { return },
                () = handle.input.closed() => return,
            }
        }
        let Some(first) = handle.output.recv().await else {
            return;
        };
        let mut buf = Vec::with_capacity(first.len().max(4096));
        buf.extend_from_slice(&first);
        while buf.len() < MAX_CHUNK {
            match handle.output.try_recv() {
                Ok(more) => buf.extend_from_slice(&more),
                Err(_) => break,
            }
        }
        sent = sent.saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX));
        if channel.send(InvokeResponseBody::Raw(buf)).is_err() {
            // The view is gone: dropping the handle closes the channel.
            return;
        }
    }
}
