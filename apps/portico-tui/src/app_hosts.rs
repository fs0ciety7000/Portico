//! App glue for hosts created in Portico (phase 6b): "Add host…", "Edit
//! host…", "Duplicate as Portico host…", "Delete host", the reload on
//! `EngineEvent::HostsChanged` and the `Include` banner.
//!
//! - The form ([`crate::host_form`]) is pure; engine calls run in tasks and
//!   come back as [`Action`]s. Refusals are routed to a field by their
//!   **code**, never by their text.
//! - `HostsChanged` reloads the inventory in place (no spinner) and keeps
//!   the selection on its host; after a save, the saved alias is selected.
//! - Portico never edits `~/.ssh/config`: when Portico hosts exist and it
//!   does not include `hosts.conf` yet, a banner gives the line to add
//!   (copyable with OSC 52, hidden until the next launch on request).

use portico_core::ErrorSummary;
use portico_core::hosts::HostOrigin;

use crate::action::Action;
use crate::app::{App, Overlay};
use crate::dialog::{ConfirmAction, ConfirmDialog, Dialog, SecretDialog, SecretPurpose};
use crate::host_form::{
    DELETE_COMMAND, FormMode, FormOutcome, HostForm, HostRequest, read_only_hint,
};
use crate::ui::fmt;

/// The proposed `Include` line and whether to show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IncludeBanner {
    /// `~/.ssh/config` already includes `hosts.conf` (`None`: unknown).
    pub(crate) active: Option<bool>,
    /// Hidden by the user for this run.
    pub(crate) hidden: bool,
    /// `Include ~/.config/portico/hosts.conf`.
    pub(crate) line: String,
}

/// The banner's first sentence (shared with the GUI and the CLI).
pub(crate) const INCLUDE_BANNER: &str = "Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config:";

/// Error codes whose message starts with a host alias: shown as written
/// (never sentence-cased, the alias keeps its case).
pub(crate) const ALIAS_FIRST_CODES: &[&str] = &["host_read_only", "host_in_use"];

/// `user@host:port` of `host` (the local user when none is set).
pub(crate) fn account(host: &portico_core::hosts::HostEntry, local_user: &str) -> String {
    let resolved = &host.resolved;
    format!(
        "{}@{}:{}",
        resolved.user.as_deref().unwrap_or(local_user),
        resolved.hostname,
        resolved.port
    )
}

/// The delete confirmation's body (shared with the GUI).
pub(crate) fn delete_body(alias: &str, account: &str) -> String {
    format!(
        "Delete {alias} ({account}) and its tags and notes. Its remembered password is forgotten unless another host uses the same account. Open sessions stay connected. This cannot be undone."
    )
}

/// A save refused for want of a vault (none yet, or locked).
fn needs_vault(error: &ErrorSummary) -> bool {
    matches!(error.code.as_str(), "vault_missing" | "vault_locked")
}

/// Spawns `fut` and sends its result back as an action. Outside a runtime
/// (pure state tests) nothing is spawned: the app always runs in one.
fn spawn<F>(app: &App, fut: F)
where
    F: Future<Output = Action> + Send + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let tx = app.tx.clone();
    runtime.spawn(async move {
        let _ = tx.send(fut.await);
    });
}

impl App {
    /// Phase 6b actions (then `update_updates`, the last link of the
    /// `update` chain).
    pub(crate) fn update_hosts(&mut self, action: Action) {
        match action {
            Action::AddHost => self.open_host_form(None),
            Action::DuplicateHost => {
                if let Some(alias) = self.selected_host().map(|h| h.alias.clone()) {
                    self.show_form(HostForm::duplicate(&alias));
                }
            }
            Action::DeleteHost => self.ask_delete(),
            Action::HostFormKey(key) => self.host_form_key(key),
            Action::HostFormKeys(result) => {
                let home = self.engine.config().paths.home.clone();
                match (result, &mut self.host_form) {
                    (Ok(keys), Some(form)) => form.set_keys(&keys, &home),
                    // Without the list, the picker keeps `none` (and the
                    // host's own key): the form stays usable.
                    (Err(_), Some(form)) => form.keys_loaded = true,
                    (_, None) => {}
                }
            }
            Action::HostFormManaged { alias, result } => self.host_form_managed(&alias, result),
            Action::HostSaved(result) => self.host_saved(result),
            Action::HostDeleted { alias, result } => match result {
                Ok(()) => self.notify(format!("Deleted {}", fmt::truncate(&alias, 48))),
                Err(e) => {
                    self.pending_select = None;
                    if ALIAS_FIRST_CODES.contains(&e.code.as_str()) {
                        self.notify_error_as_written(e);
                    } else {
                        self.notify_error(e);
                    }
                }
            },
            Action::IncludeChecked(result) => self.include.active = result.ok(),
            // Only while the banner shows them.
            Action::CopyInclude if self.include_banner() => {
                self.osc52(&self.include.line);
                self.notify("Include line copied".into());
            }
            Action::HideInclude if self.include_banner() => {
                self.include.hidden = true;
                self.notify("Hidden until Portico restarts · the line stays in hosts.conf".into());
            }
            other => self.update_updates(other),
        }
    }

    /// The `Include` banner is shown: Portico hosts exist, `~/.ssh/config`
    /// does not include `hosts.conf`, and it was not hidden.
    pub(crate) fn include_banner(&self) -> bool {
        !self.include.hidden && self.include.active == Some(false) && self.has_portico_hosts()
    }

    fn has_portico_hosts(&self) -> bool {
        self.inventory
            .as_ref()
            .is_some_and(|i| i.hosts.iter().any(|h| h.origin == HostOrigin::Portico))
    }

    /// The inventory changed on disk (`EngineEvent::HostsChanged`, or a
    /// lagged bus): reload it in place, keeping the selected host.
    pub(crate) fn on_hosts_changed(&mut self) {
        if self.pending_select.is_none() {
            self.pending_select = self.selected_host().map(|h| h.alias.clone());
        }
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::HostsLoaded(engine.load_hosts().await.map_err(|e| e.summary().message))
        });
    }

    /// After `HostsLoaded`: the host to select, then the `Include` check.
    pub(crate) fn hosts_reloaded(&mut self) {
        if let Some(alias) = self.pending_select.take() {
            self.select_alias(&alias);
        }
        self.include.line = self.engine.managed_include_line();
        if self.has_portico_hosts() {
            let engine = self.engine.clone();
            spawn(self, async move {
                Action::IncludeChecked(
                    engine
                        .managed_include_active()
                        .await
                        .map_err(|e| e.summary()),
                )
            });
        }
    }

    /// Selects `alias` (outside the Recent section when it is listed
    /// twice: that is where it was edited). Returns whether it is listed.
    fn select_alias(&mut self, alias: &str) -> bool {
        let Some(hosts) = self.inventory.as_ref().map(|i| &i.hosts) else {
            return false;
        };
        let Some(index) = hosts.iter().position(|h| h.alias == alias) else {
            return false;
        };
        let rows: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.host() == Some(index))
            .map(|(row, _)| row)
            .collect();
        let row = rows
            .iter()
            .copied()
            .find(|&r| {
                !matches!(
                    self.entries.get(r),
                    Some(crate::host_list::Entry::Host { recent: true, .. })
                )
            })
            .or_else(|| rows.first().copied());
        match row {
            Some(row) => {
                self.list.select(Some(row));
                true
            }
            None => false,
        }
    }

    /// Every alias known, for the `ProxyJump` picker.
    fn aliases(&self) -> Vec<String> {
        self.inventory
            .as_ref()
            .map(|i| i.hosts.iter().map(|h| h.alias.clone()).collect())
            .unwrap_or_default()
    }

    /// `e`: the form on a Portico host, the read-only view on another.
    pub(crate) fn edit_host(&mut self) {
        let Some(host) = self.selected_host() else {
            self.notify("Select a host to edit".into());
            return;
        };
        let alias = host.alias.clone();
        self.open_host_form(Some(alias));
    }

    /// "Add host…" (`None`) or "Edit host…" on `alias`.
    fn open_host_form(&mut self, alias: Option<String>) {
        let aliases = self.aliases();
        let form = match alias.and_then(|a| {
            self.inventory
                .as_ref()
                .and_then(|i| i.hosts.iter().find(|h| h.alias == a))
        }) {
            None => HostForm::create(&aliases),
            Some(host) if host.origin == HostOrigin::Portico => HostForm::edit(host, &aliases),
            Some(host) => HostForm::read_only(&host.alias, host.origin),
        };
        let wants_keys = matches!(form.mode, FormMode::Create | FormMode::Edit { .. });
        let edited = match &form.mode {
            FormMode::Edit { original } => Some(original.clone()),
            _ => None,
        };
        self.show_form(form);
        if let Some(alias) = edited {
            // The values hosts.conf holds, read off the UI thread.
            let engine = self.engine.clone();
            spawn(self, async move {
                let result = engine
                    .managed_host_details(&alias)
                    .await
                    .map_err(|e| e.summary());
                Action::HostFormManaged { alias, result }
            });
        }
        if wants_keys {
            let engine = self.engine.clone();
            spawn(self, async move {
                Action::HostFormKeys(engine.local_keys().await.map_err(|e| e.summary()))
            });
        }
    }

    /// The edited host's own values came back: fill the form, or close it
    /// when the host is gone from `hosts.conf` (or the file is unreadable).
    fn host_form_managed(
        &mut self,
        alias: &str,
        result: Result<Option<portico_core::managed_hosts::ManagedHostDetails>, ErrorSummary>,
    ) {
        let waiting = self.host_form.as_ref().is_some_and(|form| {
            form.loading && matches!(&form.mode, FormMode::Edit { original } if original == alias)
        });
        if !waiting {
            return;
        }
        let home = self.engine.config().paths.home.clone();
        match result {
            Ok(Some(host)) => {
                if let Some(form) = &mut self.host_form {
                    form.set_managed(&host, &home);
                }
            }
            Ok(None) => {
                self.close_form();
                self.notify_error(ErrorSummary {
                    code: "host_not_managed".into(),
                    message: format!("{} is no longer in hosts.conf", fmt::truncate(alias, 48)),
                    hint: Some(
                        "It was deleted or renamed meanwhile: pick it again in the list.".into(),
                    ),
                });
            }
            Err(error) => {
                self.close_form();
                self.notify_error(error);
            }
        }
    }

    fn show_form(&mut self, form: HostForm) {
        self.prefix = false;
        self.close_panels();
        self.host_form = Some(form);
        self.overlay = Some(Overlay::HostForm);
    }

    fn close_form(&mut self) {
        self.host_form = None;
        if self.overlay == Some(Overlay::HostForm) {
            self.overlay = None;
        }
    }

    fn host_form_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(form) = &mut self.host_form else {
            return;
        };
        match form.handle_key(key) {
            FormOutcome::None => {}
            FormOutcome::Close => self.close_form(),
            FormOutcome::EditMeta(alias) => {
                self.close_form();
                self.open_edit_meta(&alias);
            }
            FormOutcome::Save(request) => self.save_host(request),
        }
    }

    fn save_host(&self, request: HostRequest) {
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = match request {
                HostRequest::Create(draft) => engine.create_host(draft).await,
                HostRequest::Update { original, draft } => {
                    engine.update_host(&original, draft).await
                }
                HostRequest::Duplicate { source, new_alias } => {
                    engine.duplicate_as_managed(&source, &new_alias).await
                }
            };
            Action::HostSaved(result.map_err(|e| e.summary()))
        });
    }

    fn host_saved(
        &mut self,
        result: Result<portico_core::managed_hosts::HostSummary, ErrorSummary>,
    ) {
        let open = self
            .host_form
            .as_ref()
            .filter(|f| f.busy)
            .map(|f| f.mode.clone());
        match result {
            Ok(summary) => {
                let saved = summary.host.alias;
                let message = match open {
                    Some(FormMode::Edit { .. }) => format!("Saved {saved} · hosts.conf"),
                    Some(FormMode::Duplicate { .. }) => format!(
                        "Duplicated as {saved} · {} edits it",
                        self.keymap.shown("hosts.edit", 1)
                    ),
                    _ => format!("Added {saved} · hosts.conf"),
                };
                if open.is_some() {
                    self.close_form();
                }
                // Selected now if the reload already ran, else once it does.
                if !self.select_alias(&saved) {
                    self.pending_select = Some(saved);
                }
                self.notify(message);
            }
            Err(error) => {
                let home = self.engine.config().paths.home.clone();
                match &mut self.host_form {
                    // A password to remember and no vault (or a locked
                    // one): create or unlock it, then save again.
                    Some(form) if form.busy && needs_vault(&error) && form.sets_password() => {
                        let purpose = if error.code == "vault_missing" {
                            SecretPurpose::VaultCreate { offered: true }
                        } else {
                            SecretPurpose::VaultUnlock { startup: false }
                        };
                        form.wait_for_vault(error);
                        self.dialogs
                            .push_back(Dialog::Secret(SecretDialog::for_host_save(purpose)));
                    }
                    Some(form) if form.busy => form.refused(error, &home),
                    _ => self.notify_error(error),
                }
            }
        }
    }

    /// The vault dialog opened by a host save closed: the vault is ready
    /// (`true`: save again) or the user said "not now" (back to the form,
    /// the password still typed, the refusal under it).
    pub(crate) fn host_save_vault_done(&mut self, ready: bool) {
        let home = self.engine.config().paths.home.clone();
        let Some(form) = &mut self.host_form else {
            return;
        };
        if ready {
            if let Some(request) = form.retry_after_vault() {
                self.save_host(request);
            }
        } else {
            form.vault_declined(&home);
        }
    }

    /// `Delete`: an armed confirmation for a Portico host, the read-only
    /// hint for any other.
    fn ask_delete(&mut self) {
        let Some(host) = self.selected_host() else {
            return;
        };
        let (alias, origin) = (host.alias.clone(), host.origin);
        let account = account(host, &self.engine.config().local_user);
        if origin != HostOrigin::Portico {
            // The alias keeps its case: never sentence-cased.
            self.notify_warning_as_written(
                format!("{} is read-only", fmt::truncate(&alias, 32)),
                Some(read_only_hint(origin).into()),
            );
            return;
        }
        self.dialogs.push_back(Dialog::Confirm(ConfirmDialog {
            title: format!("{DELETE_COMMAND} {}?", fmt::truncate(&alias, 40)),
            subject: None,
            body: delete_body(&fmt::truncate(&alias, 40), &fmt::truncate(&account, 48)),
            confirm: "delete",
            action: ConfirmAction::DeleteHost(alias),
            armed: Some(std::time::Instant::now()),
            answers: crate::dialog::Answers::YesNo,
        }));
    }

    /// The confirmation said `y`: delete, and select the next host.
    pub(crate) fn delete_host(&mut self, alias: String) {
        self.pending_select = self.neighbour_of(&alias);
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine.delete_host(&alias).await.map_err(|e| e.summary());
            Action::HostDeleted { alias, result }
        });
    }

    /// The host listed after `alias` (else before it).
    fn neighbour_of(&self, alias: &str) -> Option<String> {
        let hosts = &self.inventory.as_ref()?.hosts;
        let listed: Vec<&str> = self
            .entries
            .iter()
            .filter_map(crate::host_list::Entry::host)
            .filter_map(|i| hosts.get(i))
            .map(|h| h.alias.as_str())
            .collect();
        let at = listed.iter().position(|a| *a == alias)?;
        listed
            .iter()
            .skip(at + 1)
            .chain(listed.iter().take(at).rev())
            .find(|a| **a != alias)
            .map(|a| (*a).to_owned())
    }
}

#[cfg(test)]
mod tests;
