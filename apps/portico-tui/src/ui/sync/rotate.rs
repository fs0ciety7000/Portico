//! Views of the key rotation dialogs (phase 11, S2c-1): "Rotate keys…",
//! "Forget <device>…", "Keep this device's keys", the adoption ("Enter the
//! new passphrase"), "Use the other device's keys", and the questions only
//! `y` answers (restore the key file, remove a newer file, remove an old
//! key). Words are the GUI's, word for word where both show them.

use portico_core::sync::api::{ForgetPreview, RotationPreview};
use portico_core::sync::{AlertKind, SyncStatus};
use ratatui::text::{Line, Span};

use super::{loading, ok, paragraph, pass_fields, pass_fields_sized, question, warning};
use crate::form::display_safe;
use crate::keys::Load;
use crate::sync_panel::{
    ADOPT_UNSURE_AFTER_R1, Ask, NEWER_FILE_HINT, OLD_KEY_CONSEQUENCE, PassForm, PassKind,
    TYPE_NOTHING, new_keys_title, subject_label,
};
use crate::sync_rotate::{RotateFlow, RotateKind, RotateStep};
use crate::theme::Styles;
use crate::ui::form;

/// The rotation's answer was lost to a folder timeout (A4), as in the GUI.
pub(crate) const CHECKING: &str = "Result unknown — Portico is checking";

/// Records listed by name in the forget preview (then "+N more").
const LAST_WRITTEN_SHOWN: usize = 5;

/// The cloud folder reminder of a forget (RR7), word for word as in the GUI.
pub(crate) fn cloud_title(label: &str) -> String {
    format!("Also remove {label}'s access to your cloud folder")
}
pub(crate) const CLOUD_TEXT: &str = "Its Dropbox, iCloud or OneDrive session, or its Syncthing share: only that closes the remaining window. Without it, the lost device can still put old files back.";
/// [`CLOUD_TEXT`] in one line, so the reminder takes two at 80 columns (B9).
pub(crate) const CLOUD_TEXT_SHORT: &str =
    "Its cloud or Syncthing session: only that closes the remaining window.";
/// Below this width, the cloud folder reminder takes its short form.
const CLOUD_FULL_AT: usize = 90;

/// `Forgotten: X. It cannot read what is synced from now on.`, word for
/// word as in the GUI (B8).
pub(crate) fn forgotten_result(labels: &[String]) -> String {
    format!(
        "Forgotten: {}. {} read what is synced from now on.",
        list_text(labels),
        if labels.len() == 1 {
            "It cannot"
        } else {
            "They cannot"
        }
    )
}

/// The result's RR7 reminder, word for word as in the GUI (B8).
pub(crate) fn cloud_reminder(labels: &[String]) -> String {
    format!(
        "{}, if not done yet: only that closes the remaining window.",
        cloud_title(&list_text(labels))
    )
}

/// `a, b and c`.
fn list_text(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `workstation (a07d51c2) — update Portico first` for a device on
/// Portico < 0.7 (B9), as in the GUI.
pub(crate) fn device_to_update(label: &str, outdated: &[String]) -> String {
    if outdated.iter().any(|o| o == label) {
        format!("{label} — update Portico first")
    } else {
        label.to_owned()
    }
}

/// R17, word for word as in the GUI.
pub(crate) const KEEP_MINE_TEXT: &str = "This device rotates the keys again from its own. The other rotation's removals are lost: if the other device removed a device, remove it again here. The other devices then see a conflict and choose in turn.";

fn blank() -> Line<'static> {
    Line::from("")
}

fn bullets(s: Styles, items: &[String], w: usize) -> Vec<Line<'static>> {
    items
        .iter()
        .flat_map(|item| {
            crate::ui::fmt::wrap(&display_safe(item), w.saturating_sub(4))
                .into_iter()
                .enumerate()
                .map(|(i, l)| {
                    Line::from(vec![
                        Span::styled(if i == 0 { "  · " } else { "    " }, s.muted()),
                        Span::styled(l, s.primary()),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `▲ text` in the warning colour, wrapped under its mark.
pub(super) fn warn(s: Styles, text: &str, w: usize) -> Vec<Line<'static>> {
    crate::ui::fmt::wrap(text, w.saturating_sub(2))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            Line::from(vec![
                Span::styled(if i == 0 { "▲ " } else { "  " }, warning(s)),
                Span::styled(l, s.primary()),
            ])
        })
        .collect()
}

/// The devices of a preview that will need the new passphrase, and the
/// Portico < 0.7 warning (Q5).
fn devices_block(
    s: Styles,
    preview: &RotationPreview,
    update: &[String],
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if update.is_empty() {
        lines.extend(paragraph(
            s.secondary(),
            "No other device to update: this is the only device of the sync.",
            w,
        ));
    } else {
        lines.extend(paragraph(
            s.secondary(),
            "Then type the new passphrase once on:",
            w,
        ));
        let shown: Vec<String> = update
            .iter()
            .map(|label| {
                let waiting = preview
                    .devices
                    .iter()
                    .any(|d| d.label == *label && d.waiting);
                if waiting && !preview.outdated.contains(label) {
                    format!("{label} — some of its data is still on its way")
                } else {
                    device_to_update(label, &preview.outdated)
                }
            })
            .collect();
        lines.extend(bullets(s, &shown, w));
    }
    if !preview.outdated.is_empty() {
        lines.push(blank());
        lines.extend(warn(
            s,
            &format!(
                "Portico older than 0.7 on {}: it stops syncing until it is updated.",
                display_safe(&preview.outdated.join(", "))
            ),
            w,
        ));
    }
    if preview.format_upgrade {
        lines.extend(paragraph(
            s.muted(),
            "The key file moves to format 2: Portico 0.6 cannot read it.",
            w,
        ));
    }
    if !preview.exposed_accounts.is_empty() {
        lines.push(blank());
        lines.extend(warn(
            s,
            "These server passwords were shared: change them after the rotation.",
            w,
        ));
        lines.extend(bullets(s, &preview.exposed_accounts, w));
    }
    lines
}

/// The labels of the other devices of a rotation preview.
fn others(preview: &RotationPreview) -> Vec<String> {
    preview
        .devices
        .iter()
        .filter(|d| !d.this_device)
        .map(|d| d.label.clone())
        .collect()
}

fn forget_block(
    s: Styles,
    label: &str,
    forget: &ForgetPreview,
    preview: &RotationPreview,
    w: usize,
) -> Vec<Line<'static>> {
    let label = display_safe(label);
    let mut lines = warn(s, &cloud_title(&label), w);
    let cloud = if w < CLOUD_FULL_AT {
        CLOUD_TEXT_SHORT
    } else {
        CLOUD_TEXT
    };
    lines.extend(
        crate::ui::fmt::wrap(cloud, w.saturating_sub(2))
            .into_iter()
            .map(|l| Line::from(vec![Span::raw("  "), Span::styled(l, s.secondary())])),
    );
    lines.push(blank());
    lines.extend(paragraph(
        s.secondary(),
        &format!(
            "Forgetting {label} rotates the keys: it cannot read anything synced from now on. What it already synced stays readable to it."
        ),
        w,
    ));
    lines.push(Line::from(vec![
        Span::styled("Last seen  ", s.muted()),
        Span::styled(super::ago(forget.last_seen), s.secondary()),
    ]));
    if !forget.last_written.is_empty() {
        lines.push(blank());
        lines.extend(paragraph(
            s.secondary(),
            "It wrote the current value of these records (they stay):",
            w,
        ));
        let mut names: Vec<String> = forget
            .last_written
            .iter()
            .take(LAST_WRITTEN_SHOWN)
            .map(subject_label)
            .collect();
        let more = forget.last_written.len().saturating_sub(LAST_WRITTEN_SHOWN);
        if more > 0 {
            names.push(format!("+{more} more"));
        }
        lines.extend(bullets(s, &names, w));
    }
    lines.push(blank());
    lines.extend(devices_block(
        s,
        preview,
        &forget.enter_new_passphrase_on,
        w,
    ));
    if !forget.exposed_accounts.is_empty() {
        lines.push(blank());
        lines.extend(warn(
            s,
            "It knew these server passwords: change them on the servers.",
            w,
        ));
        lines.extend(bullets(s, &forget.exposed_accounts, w));
    }
    lines
}

fn about_lines(flow: &RotateFlow, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let preview = match &flow.preview {
        Load::Loading => return vec![loading(s, tick, "Reading what this affects…")],
        Load::Failed(e) => return form::notice(s, false, &e.message, e.hint.as_deref(), w),
        Load::Ready(p) => p,
    };
    match &flow.kind {
        RotateKind::Rotate => {
            let mut lines = paragraph(
                s.secondary(),
                "Rotating makes new keys under a new passphrase. Use it when a passphrase or a device may have leaked: a device without the new passphrase cannot read anything synced from now on. What was synced before stays readable to whoever had the old keys.",
                w,
            );
            lines.push(blank());
            lines.extend(devices_block(s, preview, &others(preview), w));
            lines
        }
        RotateKind::Forget { label, .. } => match &flow.forget {
            Some(Load::Ready(forget)) => forget_block(s, label, forget, preview, w),
            Some(Load::Failed(e)) => form::notice(s, false, &e.message, e.hint.as_deref(), w),
            _ => vec![loading(s, tick, "Reading what this affects…")],
        },
        RotateKind::KeepMine => {
            let mut lines = warn(s, KEEP_MINE_TEXT, w);
            lines.push(blank());
            lines.extend(devices_block(s, preview, &others(preview), w));
            lines
        }
    }
}

fn done_lines(flow: &RotateFlow, s: Styles, w: usize) -> Vec<Line<'static>> {
    let Some(result) = &flow.result else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let outdated = flow
        .preview
        .ready()
        .map(|p| p.outdated.clone())
        .unwrap_or_default();
    if result.pending() {
        lines.extend(warn(s, crate::sync_panel::ROTATION_PENDING, w));
        lines.extend(paragraph(
            s.secondary(),
            "The new keys are saved on this device: Portico writes them to the folder as soon as it answers. Nothing goes back to the old keys.",
            w,
        ));
    } else {
        lines.push(Line::from(vec![
            Span::styled("✓ ", ok(s)),
            Span::styled("Keys rotated.", s.title()),
        ]));
    }
    lines.push(blank());
    if let Some(outcome) = &result.outcome {
        let forgotten: Vec<String> = outcome.forgotten.iter().map(|l| display_safe(l)).collect();
        if !forgotten.is_empty() {
            lines.extend(paragraph(s.primary(), &forgotten_result(&forgotten), w));
        }
        if outcome.enter_new_passphrase_on.is_empty() {
            lines.extend(paragraph(s.secondary(), "No other device to update.", w));
        } else {
            lines.extend(paragraph(s.primary(), "Enter the new passphrase on:", w));
            let shown: Vec<String> = outcome
                .enter_new_passphrase_on
                .iter()
                .map(|label| device_to_update(label, &outdated))
                .collect();
            lines.extend(bullets(s, &shown, w));
        }
        if !outcome.exposed_accounts.is_empty() {
            lines.push(blank());
            lines.extend(warn(s, "Change these server passwords:", w));
            lines.extend(bullets(s, &outcome.exposed_accounts, w));
        }
        if !forgotten.is_empty() {
            lines.push(blank());
            lines.extend(warn(s, &cloud_reminder(&forgotten), w));
        }
    } else if flow.kind != RotateKind::KeepMine {
        // The folder's answer was lost (A4), the status shows the keys moved.
        lines.extend(paragraph(
            s.secondary(),
            "The folder answered late, but the keys did change. The Devices tab lists the devices that need the new passphrase.",
            w,
        ));
    } else {
        lines.extend(paragraph(
                s.secondary(),
                "This device keeps its keys. On the other devices, type this device's new passphrase, or keep theirs in turn.",
                w,
            ));
        lines.extend(paragraph(
            s.muted(),
            "If the other device removed a device, remove it again here (Devices, f).",
            w,
        ));
    }
    lines
}

/// The last question (only `y` answers it).
fn confirm_lines(flow: &RotateFlow, s: Styles, w: usize) -> Vec<Line<'static>> {
    let (q, text) = match &flow.kind {
        _ if flow.without_passphrase() => (
            "Rotate the keys without the current passphrase?".to_owned(),
            "Whoever holds this unlocked device can do it: that is why it asks. y rotates, n goes back.",
        ),
        RotateKind::Rotate => (
            "Rotate the keys now?".to_owned(),
            "Every other device stops syncing until the new passphrase is typed there. y rotates, n goes back.",
        ),
        RotateKind::Forget { label, .. } => (
            format!("Forget {} and rotate the keys?", display_safe(label)),
            "It cannot read anything synced from now on. Every other device needs the new passphrase. y forgets it, n goes back.",
        ),
        RotateKind::KeepMine => (
            "Keep this device's keys and rotate again?".to_owned(),
            "The other rotation's removals are lost. y keeps them, n goes back.",
        ),
    };
    let mut lines = vec![question(s, &q)];
    lines.extend(paragraph(s.secondary(), text, w));
    lines
}

/// The rotation dialog's body, and the line to keep in view.
pub(super) fn rotate_lines(
    flow: &RotateFlow,
    s: Styles,
    tick: u64,
    w: usize,
    compact: bool,
) -> (Vec<Line<'static>>, Option<usize>) {
    let (step, steps) = match (&flow.kind, flow.step) {
        (RotateKind::KeepMine, RotateStep::About) => (1, 2),
        (RotateKind::KeepMine, _) => (2, 2),
        (_, RotateStep::About) => (1, 3),
        (_, RotateStep::Authorize) => (2, 3),
        _ => (3, 3),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(flow.title(), s.title()),
        Span::styled(
            if matches!(
                flow.step,
                RotateStep::Done | RotateStep::Working | RotateStep::Checking
            ) {
                String::new()
            } else {
                format!("  ·  {step} of {steps}")
            },
            s.muted(),
        ),
    ])];
    lines.push(blank());
    let mut focus = None;
    match flow.step {
        RotateStep::About => lines.extend(about_lines(flow, s, tick, w)),
        RotateStep::Authorize => {
            lines.extend(paragraph(
                s.secondary(),
                "Type the current sync passphrase. If nobody knows it any more, check “I no longer know it”: this device's key then authorizes the rotation, after a confirmation like a reset.",
                w,
            ));
            lines.push(blank());
            lines.extend(pass_fields(&flow.auth, s, tick, w));
            if let Some(e) = &flow.problem {
                lines.push(blank());
                lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
                focus = Some(lines.len().saturating_sub(1));
            }
        }
        RotateStep::Checking => {
            lines.extend(warn(s, CHECKING, w));
            lines.push(loading(s, tick, "Reading the sync status again…"));
            lines.push(blank());
            lines.extend(paragraph(
                s.secondary(),
                "The folder did not answer in time. The rotation may have gone through: Portico reads the status again before asking for anything. Esc closes this; the status shows the result.",
                w,
            ));
        }
        RotateStep::New | RotateStep::Confirm(_) => {
            lines.extend(paragraph(
                s.secondary(),
                "Choose the new passphrase. Every other device needs it once. Generate one, or type a long one: Portico refuses a weak one, and one it opened before.",
                w,
            ));
            lines.push(blank());
            let fields = pass_fields_sized(&flow.new, s, tick, w, compact);
            let saved_focused = flow.new.focused() == Some(crate::sync_panel::PassField::Saved);
            lines.extend(fields);
            if saved_focused {
                focus = Some(lines.len().saturating_sub(1));
            }
            if let Some(e) = &flow.problem {
                lines.push(blank());
                lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
                focus = Some(lines.len().saturating_sub(1));
            }
            if let RotateStep::Confirm(_) = flow.step {
                lines.push(blank());
                lines.extend(confirm_lines(flow, s, w));
                focus = Some(lines.len().saturating_sub(1));
            }
        }
        RotateStep::Working => {
            lines.push(loading(
                s,
                tick,
                "Rotating the keys: deriving the new key, then writing to the folder…",
            ));
            lines.push(blank());
            lines.extend(paragraph(
                s.muted(),
                "Deriving a key takes a few seconds on purpose. If the folder does not answer, the rotation finishes later, forward only.",
                w,
            ));
        }
        RotateStep::Done => lines.extend(done_lines(flow, s, w)),
    }
    (lines, focus)
}

/// "Enter the new passphrase" (adoption, §13.3.5), as in the GUI (B4):
/// who reports new keys, R1 in the primary text under its mark, then
/// what to do when nobody here changed them; "remove the file…" only when
/// a file claims newer keys (`newer`).
pub(super) fn adopt_lines(
    form_state: &PassForm,
    status: Option<&SyncStatus>,
    newer: bool,
    s: Styles,
    tick: u64,
    w: usize,
) -> Vec<Line<'static>> {
    let device = match status {
        Some(SyncStatus::Alert {
            kind: AlertKind::KeysRotatedElsewhere,
            device: Some(device),
        }) => Some(display_safe(device)),
        _ => None,
    };
    let mut lines = vec![
        Line::from(Span::styled("Enter the new passphrase", s.title())),
        blank(),
        Line::from(Span::styled(new_keys_title(device.as_deref()), s.primary())),
    ];
    lines.extend(paragraph(
        s.secondary(),
        "Enter the NEW passphrase chosen there.",
        w,
    ));
    lines.extend(warn(s, TYPE_NOTHING, w));
    lines.extend(paragraph(s.secondary(), ADOPT_UNSURE_AFTER_R1, w));
    if newer {
        lines.extend(paragraph(s.secondary(), NEWER_FILE_HINT, w));
    }
    if form_state.kind == (PassKind::Adopt { previous: true }) {
        lines.push(blank());
        lines.extend(paragraph(
            s.secondary(),
            "This device does not remember its key. Type the previous passphrase first, so your local changes keep their origin; if you no longer know it, leave it empty.",
            w,
        ));
    }
    lines.push(blank());
    lines.extend(pass_fields(form_state, s, tick, w));
    lines
}

/// "Use the other device's keys" (rotation conflict, §13.3.7).
pub(super) fn use_other_lines(
    form_state: &PassForm,
    s: Styles,
    tick: u64,
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Use the other device's keys", s.title())),
        blank(),
    ];
    lines.extend(paragraph(
        s.secondary(),
        "Type the passphrase chosen on the other device. Its keys are checked like any new keys, and refused while they still include a device this device removed.",
        w,
    ));
    lines.push(blank());
    lines.extend(pass_fields(form_state, s, tick, w));
    lines
}

/// The question of an [`Ask`], under the tab it was asked from.
pub(super) fn ask_lines(ask: &Ask, s: Styles, w: usize) -> Vec<Line<'static>> {
    let (q, text) = match ask {
        Ask::Restore => (
            "Restore the key file from this device?".to_owned(),
            "bundle.toml is rewritten from the copy this device verified. Use it when the file was replaced or damaged and nobody changed the keys.".to_owned(),
        ),
        Ask::Discard { label, file, .. } => (
            format!("Remove {}?", display_safe(file)),
            format!(
                "The file of {} claims newer keys. If nobody changed the keys, remove it; a genuine file is published again by its device.",
                display_safe(label)
            ),
        ),
        Ask::RemoveKey { .. } => (
            "Remove these old keys from this keychain?".to_owned(),
            format!("{OLD_KEY_CONSEQUENCE} The key in use is never listed here."),
        ),
    };
    let mut lines = vec![blank(), question(s, &q)];
    lines.extend(paragraph(s.secondary(), &text, w));
    lines
}

/// The newer device files under the alert (RR5), the selected one marked.
pub(super) fn newer_lines(
    files: &[portico_core::sync::api::NewerEpochFile],
    selected: usize,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![blank()];
    for (i, file) in files.iter().enumerate() {
        let on = i == selected.min(files.len().saturating_sub(1));
        let text = format!(
            "{} · {} · keys {}",
            display_safe(&file.file),
            display_safe(&file.label),
            file.epoch
        );
        lines.push(Line::from(vec![
            Span::styled(if on { "▍" } else { " " }, s.accent()),
            Span::styled(
                crate::ui::fmt::truncate(&text, w.saturating_sub(2)),
                if on { s.title() } else { s.primary() },
            ),
        ]));
    }
    lines.extend(paragraph(
        s.muted(),
        "This file claims newer keys. If nobody changed the keys, remove it (x).",
        w,
    ));
    lines
}
