//! SFTP explorer state: two panes (local | remote) bound to one session,
//! a transfer queue and small inline prompts (new folder, rename, delete,
//! conflict). Engine calls run in spawned tasks and come back as
//! [`Action::Files`]; nothing here blocks the UI.
//!
//! The remote pane follows its session: while it connects the pane shows the
//! phase, and once the session is down `↵` asks the app to reconnect (then
//! [`FilesView::rebind`] reopens SFTP on the new session).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::Engine;
use portico_core::ErrorSummary;
use portico_core::events::SftpEvent;
use portico_core::ids::{SessionId, TransferId};
use portico_core::sftp::{
    Direction, EntryKind, OnConflict, RemoteEntry, TransferRequest, join_remote, list_local,
    local_destination, remote_name, safe_local_name,
};
use ratatui::widgets::ListState;
use tokio::sync::mpsc;

use crate::action::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Local,
    Remote,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Pane {
    pub(crate) cwd: String,
    pub(crate) entries: Vec<RemoteEntry>,
    pub(crate) list: ListState,
    pub(crate) loading: bool,
    pub(crate) error: Option<ErrorSummary>,
}

impl Pane {
    pub(crate) fn visible(&self, hidden: bool) -> impl Iterator<Item = &RemoteEntry> {
        self.entries
            .iter()
            .filter(move |e| hidden || !e.name.starts_with('.'))
    }

    fn selected(&self, hidden: bool) -> Option<&RemoteEntry> {
        self.visible(hidden).nth(self.list.selected()?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransferStatus {
    Queued,
    Running,
    Done,
    Skipped,
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(crate) struct TransferRow {
    pub(crate) id: TransferId,
    pub(crate) name: String,
    pub(crate) direction: Direction,
    pub(crate) done: u64,
    pub(crate) total: Option<u64>,
    pub(crate) started: Instant,
    pub(crate) status: TransferStatus,
    /// The request, kept to retry after a conflict question.
    request: TransferRequest,
}

impl TransferRow {
    /// Bytes per second since the start.
    pub(crate) fn rate(&self) -> f64 {
        let secs = self.started.elapsed().as_secs_f64();
        if secs < 0.05 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)] // display only
            let done = self.done as f64;
            done / secs
        }
    }
}

/// Inline question at the bottom of the explorer.
#[derive(Debug, Clone)]
pub(crate) enum FilesPrompt {
    NewFolder {
        side: Side,
        name: String,
    },
    Rename {
        side: Side,
        from: String,
        name: String,
    },
    Delete {
        side: Side,
        path: String,
        name: String,
    },
    Conflict {
        request: TransferRequest,
        name: String,
    },
}

impl FilesPrompt {
    /// The pane the question is about (`None` for a transfer conflict).
    fn side(&self) -> Option<Side> {
        match self {
            Self::NewFolder { side, .. }
            | Self::Rename { side, .. }
            | Self::Delete { side, .. } => Some(*side),
            Self::Conflict { .. } => None,
        }
    }
}

/// Results of background work.
#[derive(Debug)]
pub(crate) enum FilesEvent {
    Home {
        /// Answers for a previous session (before a reconnect) are dropped.
        session: SessionId,
        result: Result<String, ErrorSummary>,
    },
    Listed {
        session: SessionId,
        side: Side,
        path: String,
        result: Result<Vec<RemoteEntry>, ErrorSummary>,
    },
    /// A file operation finished; refresh `side`.
    OpDone {
        side: Side,
        result: Result<(), ErrorSummary>,
    },
    TransferStarted {
        request: TransferRequest,
        result: Result<TransferId, ErrorSummary>,
    },
}

#[derive(Debug)]
pub(crate) struct FilesView {
    pub(crate) session: SessionId,
    pub(crate) alias: String,
    pub(crate) local: Pane,
    pub(crate) remote: Pane,
    pub(crate) side: Side,
    pub(crate) hidden: bool,
    pub(crate) transfers: Vec<TransferRow>,
    pub(crate) prompt: Option<FilesPrompt>,
    /// Message shown in the footer (errors of the last operation).
    pub(crate) notice: Option<ErrorSummary>,
    engine: Engine,
    tx: mpsc::UnboundedSender<Action>,
}

/// What the app should do after a key.
pub(crate) enum KeyOutcome {
    Handled,
    /// Leave the explorer (back to the terminal or the host list).
    Leave,
    /// The remote session is down and the user asked to reconnect.
    Reconnect,
}

fn send(tx: &mpsc::UnboundedSender<Action>, event: FilesEvent) {
    let _ = tx.send(Action::Files(event));
}

impl FilesView {
    pub(crate) fn open(
        engine: Engine,
        tx: mpsc::UnboundedSender<Action>,
        session: SessionId,
        alias: String,
    ) -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
        let mut view = Self {
            session,
            alias,
            local: Pane {
                cwd: home,
                ..Pane::default()
            },
            remote: Pane {
                loading: true,
                ..Pane::default()
            },
            side: Side::Remote,
            hidden: false,
            transfers: Vec::new(),
            prompt: None,
            notice: None,
            engine,
            tx,
        };
        view.load(Side::Local, view.local.cwd.clone());
        view.open_home();
        view
    }

    /// Asks for the remote home folder (served once the session is up).
    fn open_home(&self) {
        let (engine, tx, session) = (self.engine.clone(), self.tx.clone(), self.session);
        tokio::spawn(async move {
            let result = engine.sftp_home(session).await.map_err(|e| e.summary());
            send(&tx, FilesEvent::Home { session, result });
        });
    }

    /// Moves the remote pane to a new session of the same host (after a
    /// reconnect). The local pane and the transfer history stay.
    pub(crate) fn rebind(&mut self, session: SessionId) {
        self.session = session;
        self.remote = Pane {
            loading: true,
            ..Pane::default()
        };
        if self
            .prompt
            .as_ref()
            .is_some_and(|p| p.side() != Some(Side::Local))
        {
            self.prompt = None;
        }
        self.notice = None;
        self.open_home();
    }

    pub(crate) fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        }
    }

    fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.transfers
            .iter()
            .any(|t| matches!(t.status, TransferStatus::Running | TransferStatus::Queued))
            || self.local.loading
            || self.remote.loading
    }

    fn load(&mut self, side: Side, path: String) {
        let pane = self.pane_mut(side);
        pane.loading = true;
        pane.error = None;
        let (engine, tx, session) = (self.engine.clone(), self.tx.clone(), self.session);
        tokio::spawn(async move {
            let result = match side {
                Side::Local => list_local(Path::new(&path)).await,
                Side::Remote => engine.sftp_list(session, &path).await,
            }
            .map_err(|e| e.summary());
            send(
                &tx,
                FilesEvent::Listed {
                    session,
                    side,
                    path,
                    result,
                },
            );
        });
    }

    fn refresh(&mut self, side: Side) {
        let cwd = self.pane(side).cwd.clone();
        self.load(side, cwd);
    }

    pub(crate) fn on_event(&mut self, event: FilesEvent) {
        match event {
            FilesEvent::Home { session, .. } if session != self.session => {}
            FilesEvent::Listed {
                session,
                side: Side::Remote,
                ..
            } if session != self.session => {}
            FilesEvent::Home {
                result: Ok(home), ..
            } => self.load(Side::Remote, home),
            FilesEvent::Home { result: Err(e), .. } => {
                self.remote.loading = false;
                self.remote.error = Some(e);
            }
            FilesEvent::Listed {
                side, path, result, ..
            } => {
                let hidden = self.hidden;
                let pane = self.pane_mut(side);
                pane.loading = false;
                match result {
                    Ok(entries) => {
                        // Keep the selection on a refresh of the same folder.
                        let keep = (pane.cwd == path)
                            .then(|| pane.selected(hidden).map(|e| e.name.clone()))
                            .flatten();
                        pane.cwd = path;
                        pane.entries = entries;
                        let index = keep
                            .and_then(|name| pane.visible(hidden).position(|e| e.name == name))
                            .unwrap_or(0);
                        let any = pane.visible(hidden).next().is_some();
                        pane.list.select(any.then_some(index));
                        pane.error = None;
                    }
                    Err(e) => pane.error = Some(e),
                }
            }
            FilesEvent::OpDone { side, result } => {
                if let Err(e) = result {
                    self.notice = Some(e);
                }
                self.refresh(side);
            }
            FilesEvent::TransferStarted { request, result } => match result {
                Ok(id) => {
                    let name = transfer_name(&request);
                    self.transfers.push(TransferRow {
                        id,
                        name,
                        direction: request.direction,
                        done: 0,
                        total: None,
                        started: Instant::now(),
                        status: TransferStatus::Queued,
                        request,
                    });
                }
                Err(e) => self.notice = Some(e),
            },
        }
    }

    pub(crate) fn on_sftp(&mut self, event: &SftpEvent) {
        let id = match event {
            SftpEvent::TransferStarted { transfer, .. }
            | SftpEvent::TransferProgress { transfer, .. }
            | SftpEvent::TransferFinished { transfer, .. }
            | SftpEvent::TransferSkipped { transfer }
            | SftpEvent::TransferCancelled { transfer }
            | SftpEvent::TransferFailed { transfer, .. } => *transfer,
            _ => return,
        };
        let Some(row) = self.transfers.iter_mut().find(|t| t.id == id) else {
            return;
        };
        let mut refresh = None;
        match event {
            SftpEvent::TransferStarted { total, .. } => {
                row.status = TransferStatus::Running;
                row.total = *total;
                row.started = Instant::now();
            }
            SftpEvent::TransferProgress { done, total, .. } => {
                row.done = *done;
                row.total = *total;
            }
            SftpEvent::TransferFinished { .. } => {
                row.status = TransferStatus::Done;
                refresh = Some(destination_side(row.direction));
            }
            SftpEvent::TransferSkipped { .. } => row.status = TransferStatus::Skipped,
            SftpEvent::TransferCancelled { .. } => row.status = TransferStatus::Cancelled,
            SftpEvent::TransferFailed { error, .. } if error.code == "destination_exists" => {
                // Ask instead of failing: the row is replaced by the answer.
                let request = row.request.clone();
                let name = row.name.clone();
                self.transfers.retain(|t| t.id != id);
                self.prompt = Some(FilesPrompt::Conflict { request, name });
            }
            SftpEvent::TransferFailed { error, .. } => {
                row.status = TransferStatus::Failed(error.message.clone());
            }
            _ => {}
        }
        if let Some(side) = refresh {
            self.refresh(side);
        }
    }

    // ── Keys ────────────────────────────────────────────────────────────

    /// `remote_down`: the session is failed or closed, so the remote pane
    /// offers `↵ reconnect` instead of browsing.
    pub(crate) fn handle_key(&mut self, key: KeyEvent, remote_down: bool) -> KeyOutcome {
        if self.prompt.is_some() {
            self.prompt_key(key);
            return KeyOutcome::Handled;
        }
        self.notice = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if remote_down && self.side == Side::Remote {
            match key.code {
                KeyCode::Enter => return KeyOutcome::Reconnect,
                KeyCode::Esc | KeyCode::Char('q') => return KeyOutcome::Leave,
                KeyCode::Tab | KeyCode::BackTab => self.side = Side::Local,
                _ => {}
            }
            return KeyOutcome::Handled;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return KeyOutcome::Leave,
            KeyCode::Tab | KeyCode::BackTab => self.side = self.side.other(),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(10),
            KeyCode::PageUp => self.move_by(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.enter(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.parent(),
            KeyCode::Char('.') => {
                self.hidden = !self.hidden;
                for side in [Side::Local, Side::Remote] {
                    let any = self.pane(side).visible(self.hidden).next().is_some();
                    self.pane_mut(side).list.select(any.then_some(0));
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.refresh(Side::Local);
                if !remote_down {
                    self.refresh(Side::Remote);
                }
            }
            KeyCode::F(5) | KeyCode::Char('c') => self.copy_selected(),
            KeyCode::F(7) | KeyCode::Char('n') => {
                self.prompt = Some(FilesPrompt::NewFolder {
                    side: self.side,
                    name: String::new(),
                });
            }
            KeyCode::F(2) | KeyCode::Char('r') => self.ask_rename(),
            KeyCode::F(8) | KeyCode::Delete | KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('x') => self.cancel_latest(),
            _ => {}
        }
        KeyOutcome::Handled
    }

    fn move_by(&mut self, delta: isize) {
        let hidden = self.hidden;
        let pane = self.pane_mut(self.side);
        let len = pane.visible(hidden).count();
        if len == 0 {
            return;
        }
        let current = pane.list.selected().unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(len - 1);
        pane.list.select(Some(next));
    }

    fn enter(&mut self) {
        let side = self.side;
        let Some(entry) = self.pane(side).selected(self.hidden).cloned() else {
            return;
        };
        if entry.kind == EntryKind::Dir
            || (side == Side::Remote && entry.kind == EntryKind::Symlink)
        {
            self.load(side, entry.path);
        }
    }

    fn parent(&mut self) {
        let side = self.side;
        let cwd = self.pane(side).cwd.clone();
        let parent = match side {
            Side::Local => Path::new(&cwd).parent().map(|p| p.display().to_string()),
            Side::Remote => {
                let trimmed = cwd.trim_end_matches('/');
                trimmed.rfind('/').map(|i| {
                    if i == 0 {
                        "/".to_owned()
                    } else {
                        trimmed.get(..i).unwrap_or("/").to_owned()
                    }
                })
            }
        };
        if let Some(parent) = parent.filter(|p| *p != cwd) {
            self.load(side, parent);
        }
    }

    fn copy_selected(&mut self) {
        let Some(entry) = self.pane(self.side).selected(self.hidden).cloned() else {
            return;
        };
        if entry.kind == EntryKind::Dir {
            self.notice = Some(ErrorSummary {
                code: "not_a_file".into(),
                message: "folders cannot be copied yet".into(),
                hint: Some("Copy the files inside it.".into()),
            });
            return;
        }
        let request = match self.side {
            Side::Remote => {
                // A hostile server chooses names: never let one escape the folder.
                let local = match local_destination(Path::new(&self.local.cwd), &entry.name) {
                    Ok(local) => local,
                    Err(e) => {
                        self.notice = Some(portico_core::Error::from(e).summary());
                        return;
                    }
                };
                TransferRequest {
                    direction: Direction::Download,
                    local,
                    remote: entry.path,
                    on_conflict: OnConflict::Ask,
                }
            }
            Side::Local => TransferRequest {
                direction: Direction::Upload,
                local: PathBuf::from(&entry.path),
                remote: join_remote(&self.remote.cwd, &entry.name),
                on_conflict: OnConflict::Ask,
            },
        };
        self.start(request);
    }

    fn start(&self, request: TransferRequest) {
        let (engine, tx, session) = (self.engine.clone(), self.tx.clone(), self.session);
        tokio::spawn(async move {
            let result = engine
                .transfer(session, request.clone())
                .await
                .map_err(|e| e.summary());
            send(&tx, FilesEvent::TransferStarted { request, result });
        });
    }

    fn cancel_latest(&self) {
        if let Some(row) = self
            .transfers
            .iter()
            .rev()
            .find(|t| matches!(t.status, TransferStatus::Running | TransferStatus::Queued))
        {
            let _ = self.engine.cancel_transfer(row.id);
        }
    }

    fn ask_rename(&mut self) {
        if let Some(entry) = self.pane(self.side).selected(self.hidden) {
            self.prompt = Some(FilesPrompt::Rename {
                side: self.side,
                from: entry.path.clone(),
                name: entry.name.clone(),
            });
        }
    }

    fn ask_delete(&mut self) {
        if let Some(entry) = self.pane(self.side).selected(self.hidden) {
            self.prompt = Some(FilesPrompt::Delete {
                side: self.side,
                path: entry.path.clone(),
                name: entry.name.clone(),
            });
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        if key.code == KeyCode::Esc {
            return;
        }
        match prompt {
            FilesPrompt::NewFolder { side, mut name } => match key.code {
                KeyCode::Enter => self.mkdir(side, &name),
                code => {
                    edit(&mut name, code);
                    self.prompt = Some(FilesPrompt::NewFolder { side, name });
                }
            },
            FilesPrompt::Rename {
                side,
                from,
                mut name,
            } => match key.code {
                KeyCode::Enter => self.rename(side, &from, &name),
                code => {
                    edit(&mut name, code);
                    self.prompt = Some(FilesPrompt::Rename { side, from, name });
                }
            },
            FilesPrompt::Delete { side, path, name } => match key.code {
                KeyCode::Char('y') => self.delete(side, path),
                KeyCode::Char('n') => {}
                _ => self.prompt = Some(FilesPrompt::Delete { side, path, name }),
            },
            FilesPrompt::Conflict { request, name } => {
                let policy = match key.code {
                    KeyCode::Char('o') => Some(OnConflict::Overwrite),
                    KeyCode::Char('k') => Some(OnConflict::Rename),
                    KeyCode::Char('s') => Some(OnConflict::Skip),
                    _ => None,
                };
                match policy {
                    Some(on_conflict) => self.start(TransferRequest {
                        on_conflict,
                        ..request
                    }),
                    None => self.prompt = Some(FilesPrompt::Conflict { request, name }),
                }
            }
        }
    }

    /// Validates a name typed for a new or renamed entry.
    fn valid_name(&mut self, name: &str) -> bool {
        let ok = safe_local_name(name.trim()).is_some();
        if !ok {
            self.notice = Some(ErrorSummary {
                code: "invalid_name".into(),
                message: "a name cannot be empty, contain / or be . or ..".into(),
                hint: None,
            });
        }
        ok
    }

    fn mkdir(&mut self, side: Side, name: &str) {
        if !self.valid_name(name) {
            return;
        }
        let path = self.child(side, name.trim());
        self.run_op(side, move |engine, session| async move {
            match side {
                Side::Local => tokio::fs::create_dir(&path)
                    .await
                    .map_err(|e| portico_core::Error::io(&path, e)),
                Side::Remote => engine.sftp_mkdir(session, &path).await,
            }
        });
    }

    fn rename(&mut self, side: Side, from: &str, name: &str) {
        if !self.valid_name(name) || remote_name(from) == name.trim() {
            return;
        }
        let to = self.child(side, name.trim());
        let from = from.to_owned();
        self.run_op(side, move |engine, session| async move {
            match side {
                Side::Local => {
                    if tokio::fs::symlink_metadata(&to).await.is_ok() {
                        return Err(
                            portico_core::sftp::SftpError::DestinationExists { path: to }.into(),
                        );
                    }
                    tokio::fs::rename(&from, &to)
                        .await
                        .map_err(|e| portico_core::Error::io(&from, e))
                }
                Side::Remote => engine.sftp_rename(session, &from, &to).await,
            }
        });
    }

    fn delete(&mut self, side: Side, path: String) {
        self.run_op(side, move |engine, session| async move {
            match side {
                Side::Local => {
                    let meta = tokio::fs::symlink_metadata(&path)
                        .await
                        .map_err(|e| portico_core::Error::io(&path, e))?;
                    let removed = if meta.is_dir() {
                        tokio::fs::remove_dir(&path).await
                    } else {
                        tokio::fs::remove_file(&path).await
                    };
                    removed.map_err(|e| portico_core::Error::io(&path, e))
                }
                Side::Remote => engine.sftp_remove(session, &path).await,
            }
        });
    }

    fn child(&self, side: Side, name: &str) -> String {
        match side {
            Side::Local => Path::new(&self.local.cwd).join(name).display().to_string(),
            Side::Remote => join_remote(&self.remote.cwd, name),
        }
    }

    fn run_op<F, Fut>(&self, side: Side, op: F)
    where
        F: FnOnce(Engine, SessionId) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = portico_core::Result<()>> + Send,
    {
        let (engine, tx, session) = (self.engine.clone(), self.tx.clone(), self.session);
        tokio::spawn(async move {
            let result = op(engine, session).await.map_err(|e| e.summary());
            send(&tx, FilesEvent::OpDone { side, result });
        });
    }
}

fn edit(text: &mut String, code: KeyCode) {
    match code {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char(c) if text.chars().count() < 255 => text.push(c),
        _ => {}
    }
}

fn destination_side(direction: Direction) -> Side {
    match direction {
        Direction::Download => Side::Local,
        Direction::Upload => Side::Remote,
    }
}

fn transfer_name(request: &TransferRequest) -> String {
    match request.direction {
        Direction::Download => remote_name(&request.remote).to_owned(),
        Direction::Upload => request
            .local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// `1.4 MiB`, `812 KiB`, `96 B` (IEC units).
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)] // display only
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let label = UNITS.get(unit).copied().unwrap_or("PiB");
    if value < 10.0 {
        format!("{value:.1} {label}")
    } else {
        format!("{value:.0} {label}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_iec_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(10 * 1024 * 1024), "10 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn prompt_editing() {
        let mut s = String::from("ab");
        edit(&mut s, KeyCode::Backspace);
        edit(&mut s, KeyCode::Char('z'));
        assert_eq!(s, "az");
    }
}
