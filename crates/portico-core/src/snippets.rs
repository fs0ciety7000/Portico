//! Command snippets: reusable commands, global or scoped to hosts / tags.
//!
//! Stored in `snippets.toml` (config dir) through [`SnippetStore`] (atomic
//! `0600` writes). Snippets may contain `{{placeholders}}` that the UI asks
//! for before sending.
//!
//! ## Safety model
//!
//! A snippet body is **typed into the remote PTY**, never run locally. What
//! reaches the terminal is exactly what the user saw:
//!
//! - the command may span lines (`\n`, `\t` allowed) but never contains ESC,
//!   other C0 controls, DEL, C1 controls, Unicode line/paragraph separators
//!   (U+2028/U+2029), bidi overrides or invisible format characters
//!   (zero-width space and non-joiner, word joiner and invisible operators,
//!   BOM, soft hyphen, tag characters; the zero-width joiner of emoji
//!   sequences stays allowed): an ESC could end a bracketed paste early
//!   (`ESC[201~`) and turn the rest into keystrokes, a C1 `CSI` (U+009B) is
//!   an ESC in disguise, and the others make the displayed command differ
//!   from the typed one. Checked on save, on load **and** on render (the
//!   file can be edited by hand);
//! - placeholder values are single-line: no line break (a newline in a
//!   value would execute a half-built command) and no control or invisible
//!   character;
//! - substitution is a single pass, so a value that looks like
//!   `{{other}}` is inserted literally, never expanded;
//! - a multi-line body is only pasted (`run = false`) inside a bracketed
//!   paste: without it, each line break would run a line
//!   (`snippet_input` refuses with `snippet_needs_paste_mode`).
//!
//! `snippets.toml` is read with a size cap; entries that fail validation
//! are hidden from [`SnippetStore::list`] (kept in the file, a warning is
//! logged without their content) and duplicate or malformed ids are
//! re-derived from the name.
//!
//! [`Snippet::render_checked`] enforces this; `snippet_input` (crate
//! private) turns the result into the bytes the UI writes on its
//! `TerminalHandle`. UIs get those bytes only from
//! [`crate::Engine::snippet_input_for`], which also refuses a snippet
//! changed by sync and not confirmed on this device (F8): no public path
//! skips that gate.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::hosts::HostEntry;
use crate::store::save_toml;

/// Longest snippet name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Longest description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// Longest command body, in bytes.
pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// Longest id (`[a-z0-9-]`).
pub const MAX_ID_LEN: usize = 64;
/// Longest placeholder name, in characters.
pub const MAX_PLACEHOLDER_CHARS: usize = 64;
/// Longest placeholder value, in bytes.
pub const MAX_VALUE_BYTES: usize = 4 * 1024;
/// Most hosts or tags in a scope.
pub const MAX_SCOPE_ENTRIES: usize = 256;
/// Longest host alias or tag in a scope, in bytes.
const MAX_SCOPE_ENTRY_LEN: usize = 255;
/// Largest `snippets.toml` read, in bytes.
pub const MAX_SNIPPETS_FILE_BYTES: u64 = 1024 * 1024;
/// Generated ids keep at most this much of the name (room for `-NNN`).
const MAX_SLUG_BASE: usize = 48;

/// Bracketed paste markers (xterm `DECSET 2004`).
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "values", rename_all = "snake_case")]
pub enum SnippetScope {
    Global,
    Hosts(Vec<String>),
    Tags(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// Stable slug (`[a-z0-9-]`), unique in the file. Empty when creating:
    /// [`SnippetStore::save`] derives one from the name.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    pub scope: SnippetScope,
    /// Append a newline (execute) instead of just pasting.
    #[serde(default = "default_true")]
    pub run: bool,
}

fn default_true() -> bool {
    true
}

/// Content of `snippets.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnippetFile {
    #[serde(rename = "snippet")]
    pub snippets: Vec<Snippet>,
}

/// Snippet failures. Messages name the field or placeholder, never a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SnippetError {
    #[error("invalid snippet {field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
    #[error("no value for placeholder {{{{{placeholder}}}}}")]
    MissingValue { placeholder: String },
    #[error("the value of {{{{{placeholder}}}}} {reason}")]
    Value {
        placeholder: String,
        reason: &'static str,
    },
    #[error("unknown snippet {id}")]
    NotFound { id: String },
    #[error("a multi-line snippet cannot be pasted without bracketed paste")]
    NeedsPasteMode,
    #[error("{} is larger than {max} bytes", path.display())]
    FileTooLarge { path: PathBuf, max: u64 },
}

impl SnippetError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Invalid {
                field: "command", ..
            } => (
                "snippet_invalid",
                Some(
                    "Commands may use several lines and tabs, but no escape or other control characters.",
                ),
            ),
            Self::Invalid { field: "id", .. } => (
                "snippet_invalid",
                Some(
                    "Ids use lowercase letters, digits and dashes; leave it empty to derive one from the name.",
                ),
            ),
            Self::Invalid { .. } => ("snippet_invalid", None),
            Self::MissingValue { .. } => (
                "snippet_missing_value",
                Some("Fill in every placeholder before sending the snippet."),
            ),
            Self::Value { .. } => (
                "snippet_value",
                Some(
                    "Values are typed on a single line: remove line breaks, tabs and control characters.",
                ),
            ),
            Self::NotFound { .. } => (
                "snippet_not_found",
                Some("It may have been deleted from another window: reload the snippets."),
            ),
            Self::NeedsPasteMode => (
                "snippet_needs_paste_mode",
                Some(
                    "The remote shell does not accept multi-line pastes: run the snippet, or paste it from a shell with bracketed paste.",
                ),
            ),
            Self::FileTooLarge { .. } => (
                "snippet_file_too_large",
                Some("snippets.toml is unusually large: trim it by hand or move it away."),
            ),
        }
    }
}

impl Snippet {
    pub fn applies_to(&self, host: &HostEntry) -> bool {
        self.applies_to_alias(&host.alias, &host.meta.tags)
    }

    /// Scope check from an alias and its tags (hosts that are not in the
    /// inventory have no tags).
    pub fn applies_to_alias(&self, alias: &str, tags: &[String]) -> bool {
        match &self.scope {
            SnippetScope::Global => true,
            SnippetScope::Hosts(aliases) => aliases.iter().any(|a| a == alias),
            SnippetScope::Tags(scope_tags) => scope_tags
                .iter()
                .any(|t| tags.iter().any(|ht| ht.eq_ignore_ascii_case(t))),
        }
    }

    /// Distinct `{{name}}` placeholders, in order of appearance.
    pub fn placeholders(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for token in tokens(&self.command) {
            if let Token::Placeholder(name) = token
                && !out.contains(&name)
            {
                out.push(name);
            }
        }
        out
    }

    /// Substitutes every placeholder, refusing anything that could make the
    /// terminal receive something other than the displayed command (see the
    /// module docs). Every placeholder needs a value; extra values are
    /// ignored; values are inserted literally (single pass).
    pub fn render_checked(&self, values: &[(&str, &str)]) -> Result<String> {
        check_command(&self.command)?;
        let mut out = String::with_capacity(self.command.len());
        for token in tokens(&self.command) {
            match token {
                Token::Text(text) => out.push_str(text),
                Token::Placeholder(name) => {
                    let value = values
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| *value)
                        .ok_or_else(|| SnippetError::MissingValue {
                            placeholder: name.to_owned(),
                        })?;
                    check_value(name, value)?;
                    out.push_str(value);
                }
            }
        }
        Ok(out)
    }

    /// Checks every field (the id only when set). See [`SnippetStore::save`].
    pub fn validate(&self) -> Result<(), SnippetError> {
        if !self.id.is_empty() {
            validate_id(&self.id)?;
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err(invalid("name", "is empty"));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid("name", "is too long"));
        }
        if name.chars().any(|c| c.is_control() || is_hidden_format(c)) {
            return Err(invalid("name", "contains control characters"));
        }
        if self.command.trim().is_empty() {
            return Err(invalid("command", "is empty"));
        }
        check_command(&self.command)?;
        if self
            .placeholders()
            .iter()
            .any(|p| p.chars().count() > MAX_PLACEHOLDER_CHARS)
        {
            return Err(invalid(
                "command",
                "has a placeholder name that is too long",
            ));
        }
        if let Some(description) = &self.description {
            if description.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(invalid("description", "is too long"));
            }
            if description
                .chars()
                .any(|c| (c.is_control() && c != '\n') || is_hidden_format(c))
            {
                return Err(invalid("description", "contains control characters"));
            }
        }
        match &self.scope {
            SnippetScope::Global => Ok(()),
            SnippetScope::Hosts(entries) => check_scope("hosts", entries),
            SnippetScope::Tags(entries) => check_scope("tags", entries),
        }
    }

    /// Trims the name and description, drops trailing line breaks of the
    /// command (the `run` flag decides whether Enter is sent).
    pub(crate) fn normalize(&mut self) {
        self.id = self.id.trim().to_owned();
        self.name = self.name.trim().to_owned();
        self.command = self.command.trim_end_matches(['\n', '\r']).to_owned();
        self.description = self
            .description
            .take()
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty());
        if let SnippetScope::Hosts(entries) | SnippetScope::Tags(entries) = &mut self.scope {
            for entry in entries.iter_mut() {
                *entry = entry.trim().to_owned();
            }
        }
    }
}

/// Bytes to write on the terminal input for a rendered snippet.
///
/// Line breaks become `\r` (what a terminal sends for Enter, as xterm and
/// xterm.js do on paste). With `bracketed_paste` (the remote application
/// enabled `DECSET 2004`, which the UI's terminal emulator tracks) the body is
/// wrapped in paste markers, so a multi-line snippet is inserted as a whole
/// instead of running line by line. `run` appends a final `\r`.
///
/// A multi-line body that is only to be pasted (`!run`) needs bracketed
/// paste: otherwise every line but the last would run, which is not what
/// "paste" promises. That case is refused with
/// [`SnippetError::NeedsPasteMode`] (`snippet_needs_paste_mode`).
///
/// Defensive: whatever the input, ESC and other control characters (except
/// tab and line breaks) are dropped, so the paste can never be closed early.
pub(crate) fn snippet_input(rendered: &str, run: bool, bracketed_paste: bool) -> Result<Vec<u8>> {
    let body = rendered
        .trim_end_matches(['\n', '\r'])
        .replace("\r\n", "\n");
    if !run && !bracketed_paste && body.contains(['\n', '\r']) {
        return Err(SnippetError::NeedsPasteMode.into());
    }
    let text: String = body
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some('\r'),
            '\t' => Some('\t'),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let mut out = Vec::with_capacity(
        text.len()
            .saturating_add(PASTE_START.len() + PASTE_END.len() + 1),
    );
    if bracketed_paste && !text.is_empty() {
        out.extend_from_slice(PASTE_START);
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(PASTE_END);
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    if run {
        out.push(b'\r');
    }
    Ok(out)
}

/// `snippets.toml` with serialized read-modify-write. Blocking: async
/// callers use `spawn_blocking` (the [`crate::Engine`] snippet methods do).
#[derive(Debug)]
pub struct SnippetStore {
    path: PathBuf,
    /// Serializes read-modify-write cycles within the process.
    lock: Mutex<()>,
}

impl SnippetStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every valid snippet, in file order (a missing file is an empty
    /// list). Entries that fail [`Snippet::validate`] (hand-edited file) are
    /// left out with a warning naming only their position and the failing
    /// field; they stay in the file.
    pub fn list(&self) -> Result<Vec<Snippet>> {
        self.list_with_invalid().map(|(valid, _)| valid)
    }

    /// [`Self::list`], plus the ids of the entries it leaves out (the
    /// folder sync must neither publish them nor take their absence for a
    /// deletion).
    pub(crate) fn list_with_invalid(&self) -> Result<(Vec<Snippet>, Vec<String>)> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let file = self.load()?;
        let mut invalid_ids = Vec::new();
        let mut valid = Vec::with_capacity(file.snippets.len());
        for (index, snippet) in file.snippets.into_iter().enumerate() {
            match snippet.validate() {
                Ok(()) => valid.push(snippet),
                Err(e) => {
                    invalid_ids.push(snippet.id.clone());
                    let (field, reason) = match &e {
                        SnippetError::Invalid { field, reason } => (*field, *reason),
                        _ => ("snippet", "is invalid"),
                    };
                    tracing::warn!(
                        path = %self.path.display(),
                        index,
                        field,
                        reason,
                        "ignoring an invalid snippet"
                    );
                }
            }
        }
        Ok((valid, invalid_ids))
    }

    /// Loads the file (at most [`MAX_SNIPPETS_FILE_BYTES`]). Entries with a
    /// missing, malformed or duplicate id get one derived from their name
    /// (deterministic, persisted by the next write). Validation is left to
    /// the callers: invalid entries are kept so that a write never loses
    /// what the user typed by hand.
    fn load(&self) -> Result<SnippetFile> {
        let mut file: SnippetFile = load_toml_capped(&self.path)?;
        for index in 0..file.snippets.len() {
            let earlier = file.snippets.get(..index).unwrap_or_default();
            let needs_id = file.snippets.get(index).and_then(|s| {
                let id = s.id.trim();
                let bad = id.is_empty()
                    || validate_id(id).is_err()
                    || earlier.iter().any(|other| other.id == id);
                bad.then(|| s.name.clone())
            });
            if let Some(name) = needs_id {
                let id = unique_id(&name, &file.snippets);
                if let Some(snippet) = file.snippets.get_mut(index) {
                    snippet.id = id;
                }
            }
        }
        Ok(file)
    }

    /// Creates or updates (by id) a snippet and returns it as stored. An
    /// empty id is derived from the name and made unique.
    pub fn save(&self, snippet: Snippet) -> Result<Snippet> {
        self.save_guarded(snippet, |_| Ok(()))
    }

    /// [`Self::save`], running `before_write` with the snippet as it will
    /// be stored (its final id) under the store lock, before the file is
    /// written: an error there writes nothing. The folder sync records a
    /// received snippet's confirmation gate this way **before** the
    /// snippet exists on disk (F8): a crash in between leaves at worst a
    /// gate without its snippet, never a runnable unconfirmed snippet.
    pub(crate) fn save_guarded(
        &self,
        mut snippet: Snippet,
        before_write: impl FnOnce(&Snippet) -> Result<()>,
    ) -> Result<Snippet> {
        snippet.normalize();
        snippet.validate()?;
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut file = self.load()?;
        if snippet.id.is_empty() {
            snippet.id = unique_id(&snippet.name, &file.snippets);
        }
        before_write(&snippet)?;
        match file.snippets.iter_mut().find(|s| s.id == snippet.id) {
            Some(existing) => existing.clone_from(&snippet),
            None => file.snippets.push(snippet.clone()),
        }
        save_toml(&self.path, &file)?;
        Ok(snippet)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut file = self.load()?;
        let index = file
            .snippets
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| SnippetError::NotFound { id: id.to_owned() })?;
        file.snippets.remove(index);
        save_toml(&self.path, &file)
    }
}

/// [`load_toml`] with a size cap checked before parsing.
fn load_toml_capped(path: &Path) -> Result<SnippetFile> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_SNIPPETS_FILE_BYTES => {
            return Err(SnippetError::FileTooLarge {
                path: path.to_path_buf(),
                max: MAX_SNIPPETS_FILE_BYTES,
            }
            .into());
        }
        _ => {}
    }
    // The file may grow between the check and the read: read at most one
    // byte more than the cap and check again.
    let text = {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SnippetFile::default());
            }
            Err(e) => return Err(crate::Error::io(path, e)),
        };
        let mut text = String::new();
        file.take(MAX_SNIPPETS_FILE_BYTES.saturating_add(1))
            .read_to_string(&mut text)
            .map_err(|e| crate::Error::io(path, e))?;
        text
    };
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_SNIPPETS_FILE_BYTES {
        return Err(SnippetError::FileTooLarge {
            path: path.to_path_buf(),
            max: MAX_SNIPPETS_FILE_BYTES,
        }
        .into());
    }
    toml::from_str(&text).map_err(|e| crate::Error::Serde {
        what: "toml",
        message: format!("{}: {e}", path.display()),
    })
}

// ── Parsing and checks ──────────────────────────────────────────────────

enum Token<'a> {
    Text(&'a str),
    /// Trimmed name.
    Placeholder(&'a str),
}

/// Splits a command into literal text and `{{ name }}` placeholders.
/// `{{}}`, `{{  }}` and an unterminated `{{` are literal text.
fn tokens(command: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut rest = command;
    while let Some(start) = rest.find("{{") {
        let Some(after) = rest.get(start.saturating_add(2)..) else {
            break;
        };
        let Some(end) = after.find("}}") else { break };
        let name = after.get(..end).unwrap_or_default().trim();
        let token_end = start
            .saturating_add(2)
            .saturating_add(end)
            .saturating_add(2);
        if name.is_empty() {
            out.push(Token::Text(rest.get(..token_end).unwrap_or_default()));
        } else {
            out.push(Token::Text(rest.get(..start).unwrap_or_default()));
            out.push(Token::Placeholder(name));
        }
        rest = rest.get(token_end..).unwrap_or_default();
    }
    if !rest.is_empty() {
        out.push(Token::Text(rest));
    }
    out
}

fn invalid(field: &'static str, reason: &'static str) -> SnippetError {
    SnippetError::Invalid { field, reason }
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Invisible format characters that make the displayed text differ from
/// the typed one: zero-width space and non-joiner, word joiner and
/// invisible operators, BOM / zero-width no-break space, soft hyphen and
/// tag characters. U+200D (zero-width joiner) is allowed: emoji sequences
/// use it.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{200b}'
            | '\u{200c}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

/// Bidi controls, invisible format characters and line/paragraph
/// separators: never allowed in single-line fields.
pub(crate) fn is_hidden_format(c: char) -> bool {
    is_bidi_control(c) || is_invisible_format(c) || matches!(c, '\u{2028}' | '\u{2029}')
}

fn check_command(command: &str) -> Result<(), SnippetError> {
    if command.len() > MAX_COMMAND_BYTES {
        return Err(invalid("command", "is too long"));
    }
    for c in command.chars() {
        let reason = match c {
            '\n' | '\t' => continue,
            '\u{1b}' => "contains an escape character (ESC)",
            '\u{80}'..='\u{9f}' => "contains a C1 control character",
            '\u{2028}' | '\u{2029}' => "contains a Unicode line or paragraph separator",
            c if c.is_control() => "contains a control character",
            c if is_bidi_control(c) => "contains a bidirectional text override",
            c if is_invisible_format(c) => "contains an invisible format character",
            _ => continue,
        };
        return Err(invalid("command", reason));
    }
    Ok(())
}

fn check_value(placeholder: &str, value: &str) -> Result<(), SnippetError> {
    let fail = |reason| SnippetError::Value {
        placeholder: placeholder.to_owned(),
        reason,
    };
    if value.len() > MAX_VALUE_BYTES {
        return Err(fail("is too long"));
    }
    for c in value.chars() {
        let reason = match c {
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                "contains a line break"
            }
            '\u{1b}' => "contains an escape character (ESC)",
            '\u{80}'..='\u{9f}' => "contains a C1 control character",
            c if c.is_control() => "contains a control character",
            c if is_bidi_control(c) => "contains a bidirectional text override",
            c if is_invisible_format(c) => "contains an invisible format character",
            _ => continue,
        };
        return Err(fail(reason));
    }
    Ok(())
}

fn check_scope(field: &'static str, entries: &[String]) -> Result<(), SnippetError> {
    if entries.is_empty() {
        return Err(invalid(field, "is empty (use the global scope instead)"));
    }
    if entries.len() > MAX_SCOPE_ENTRIES {
        return Err(invalid(field, "has too many entries"));
    }
    for entry in entries {
        if entry.is_empty() || entry.len() > MAX_SCOPE_ENTRY_LEN {
            return Err(invalid(field, "has an empty or too long entry"));
        }
        if entry
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_hidden_format(c))
        {
            return Err(invalid(
                field,
                "has an entry with spaces or control characters",
            ));
        }
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), SnippetError> {
    if id.len() > MAX_ID_LEN {
        return Err(invalid("id", "is too long"));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(invalid("id", "must only use a-z, 0-9 and -"));
    }
    if id.starts_with('-') || id.ends_with('-') {
        return Err(invalid("id", "must not start or end with -"));
    }
    Ok(())
}

/// `[a-z0-9-]` slug of a name, accents folded (`Redémarrer nginx` →
/// `redemarrer-nginx`). Never empty.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let folded = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
            'ç' => "c",
            'è' | 'é' | 'ê' | 'ë' => "e",
            'ì' | 'í' | 'î' | 'ï' => "i",
            'ñ' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
            'ù' | 'ú' | 'û' | 'ü' => "u",
            'ý' | 'ÿ' => "y",
            'æ' => "ae",
            'œ' => "oe",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                out.push(c);
                continue;
            }
            _ => "-",
        };
        if folded == "-" {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push_str(folded);
        }
    }
    out.truncate(MAX_SLUG_BASE);
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "snippet".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Slug of `name` not used by `existing` (`df`, `df-2`, `df-3`…).
fn unique_id(name: &str, existing: &[Snippet]) -> String {
    let base = slugify(name);
    let taken = |id: &str| existing.iter().any(|s| s.id == id);
    if !taken(&base) {
        return base;
    }
    // Among `len + 1` candidates at least one is free (pigeonhole).
    (2..=existing.len().saturating_add(2))
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(command: &str) -> Snippet {
        Snippet {
            id: String::new(),
            name: "Tail logs".into(),
            command: command.into(),
            description: None,
            scope: SnippetScope::Global,
            run: true,
        }
    }

    fn code(e: &crate::Error) -> String {
        e.summary().code
    }

    #[test]
    fn placeholders_in_order_and_distinct() {
        let s = snippet("journalctl -u {{unit}} -n {{ lines }} | grep {{unit}} {{}} {{ }} {{open");
        assert_eq!(s.placeholders(), ["unit", "lines"]);
    }

    #[test]
    fn render_checked_substitutes_everything() {
        let s = snippet("journalctl -u {{unit}} -n {{ lines }} | grep {{unit}} {{}}");
        assert_eq!(
            s.render_checked(&[("unit", "nginx"), ("lines", "50"), ("unused", "x")])
                .expect("render"),
            "journalctl -u nginx -n 50 | grep nginx {{}}"
        );
        // Empty values are allowed.
        assert_eq!(
            snippet("ls {{opts}}")
                .render_checked(&[("opts", "")])
                .expect("render"),
            "ls "
        );
        // Unicode text and tabs in the body, multi-line commands.
        let s = snippet("echo 'héllo'\tdone\nuptime");
        assert_eq!(
            s.render_checked(&[]).expect("render"),
            "echo 'héllo'\tdone\nuptime"
        );
    }

    #[test]
    fn render_checked_is_single_pass() {
        let s = snippet("echo {{a}} {{b}}");
        assert_eq!(
            s.render_checked(&[("a", "{{b}}"), ("b", "x")])
                .expect("render"),
            "echo {{b}} x"
        );
    }

    #[test]
    fn missing_placeholder_values_are_refused() {
        let s = snippet("ssh {{host}} -p {{port}}");
        let e = s.render_checked(&[("host", "db")]).expect_err("missing");
        assert_eq!(code(&e), "snippet_missing_value");
        assert!(e.to_string().contains("{{port}}"), "{e}");
        assert!(e.summary().hint.is_some());
    }

    #[test]
    fn unsafe_values_are_refused() {
        let s = snippet("grep {{pattern}} /var/log/syslog");
        for bad in [
            "a\nrm -rf ~",
            "a\r",
            "a\u{0b}b",
            "\u{2028}",
            "\u{85}",
            "\u{1b}[201~; rm -rf ~",
            "\u{9b}201~",
            "\u{07}",
            "a\tb",
            "\u{7f}",
            "\u{202e}txt.exe",
        ] {
            let e = s.render_checked(&[("pattern", bad)]).expect_err(bad);
            assert_eq!(code(&e), "snippet_value", "{bad:?}");
            assert!(e.summary().hint.is_some());
            // The value never appears in the message.
            assert!(!e.to_string().contains(bad), "{e}");
        }
        let long = "x".repeat(MAX_VALUE_BYTES + 1);
        assert!(s.render_checked(&[("pattern", &long)]).is_err());
    }

    #[test]
    fn unsafe_commands_are_refused_at_render() {
        for bad in [
            "echo \u{1b}[201~",
            "echo \u{9b}31m",
            "echo \u{1b}]52;c;aGk=\u{07}",
            "echo a\rb",
            "echo \u{04}",
            "echo \u{0}",
            "ls \u{2066}x",
        ] {
            let e = snippet(bad).render_checked(&[]).expect_err(bad);
            assert_eq!(code(&e), "snippet_invalid", "{bad:?}");
        }
    }

    #[test]
    fn validation() {
        snippet("df -h").validate().expect("valid");
        let cases: Vec<(Snippet, &str)> = vec![
            (
                Snippet {
                    name: "  ".into(),
                    ..snippet("df")
                },
                "name",
            ),
            (
                Snippet {
                    name: "x".repeat(81),
                    ..snippet("df")
                },
                "name",
            ),
            (
                Snippet {
                    name: "a\u{1b}b".into(),
                    ..snippet("df")
                },
                "name",
            ),
            (snippet(" \n "), "command"),
            (snippet(&"x".repeat(MAX_COMMAND_BYTES + 1)), "command"),
            (snippet("echo \u{1b}c"), "command"),
            (
                snippet(&format!("echo {{{{{}}}}}", "p".repeat(65))),
                "command",
            ),
            (
                Snippet {
                    id: "Bad_Id".into(),
                    ..snippet("df")
                },
                "id",
            ),
            (
                Snippet {
                    id: "-x".into(),
                    ..snippet("df")
                },
                "id",
            ),
            (
                Snippet {
                    id: "a".repeat(65),
                    ..snippet("df")
                },
                "id",
            ),
            (
                Snippet {
                    description: Some("d".repeat(501)),
                    ..snippet("df")
                },
                "description",
            ),
            (
                Snippet {
                    description: Some("\u{1b}[2J".into()),
                    ..snippet("df")
                },
                "description",
            ),
            (
                Snippet {
                    scope: SnippetScope::Hosts(vec![]),
                    ..snippet("df")
                },
                "hosts",
            ),
            (
                Snippet {
                    scope: SnippetScope::Tags(vec!["a b".into()]),
                    ..snippet("df")
                },
                "tags",
            ),
        ];
        for (s, field) in cases {
            match s.validate() {
                Err(SnippetError::Invalid { field: got, .. }) => assert_eq!(got, field, "{s:?}"),
                other => panic!("{s:?}: {other:?}"),
            }
        }
        Snippet {
            id: "ok-1".into(),
            description: Some("line\nline".into()),
            ..snippet("df")
        }
        .validate()
        .expect("valid");
    }

    fn input(rendered: &str, run: bool, bracketed_paste: bool) -> Vec<u8> {
        snippet_input(rendered, run, bracketed_paste).expect("input")
    }

    #[test]
    fn input_bytes() {
        assert_eq!(input("df -h", true, false), b"df -h\r");
        assert_eq!(input("df -h", false, false), b"df -h");
        // Trailing line breaks do not make a body multi-line.
        assert_eq!(input("df -h\n\r\n", false, false), b"df -h");
        assert_eq!(
            input("a\nb\r\nc\n\n", true, true),
            b"\x1b[200~a\rb\rc\x1b[201~\r"
        );
        // Running a multi-line body line by line is what `run` asks for.
        assert_eq!(input("a\nb", true, false), b"a\rb\r");
        assert_eq!(input("a\nb", false, true), b"\x1b[200~a\rb\x1b[201~");
        assert_eq!(input("", true, true), b"\r");
        // Defensive: control characters never get through, even unchecked.
        assert_eq!(
            input("x\u{1b}[201~y\u{9b}z\t", false, true),
            "\x1b[200~x[201~yz\t\x1b[201~".as_bytes()
        );
        assert_eq!(input("é", false, false), "é".as_bytes());
    }

    #[test]
    fn paste_only_multiline_without_bracketed_paste_is_refused() {
        for body in ["a\nb", "a\r\nb", "a\rb", "a\n\nb\n"] {
            let e = snippet_input(body, false, false).expect_err(body);
            assert_eq!(code(&e), "snippet_needs_paste_mode", "{body:?}");
            let hint = e.summary().hint.expect("hint");
            assert!(hint.contains("bracketed paste"), "{hint}");
            assert!(!e.to_string().contains(body), "{e}");
        }
    }

    #[test]
    fn command_with_line_separator_or_zero_width_is_refused() {
        for bad in [
            "ls\u{2028}rm -rf ~",
            "ls\u{2029}x",
            "rm\u{200b} -rf",
            "a\u{200c}b",
            "a\u{2060}b",
            "a\u{2063}b",
            "\u{feff}ls",
            "r\u{ad}m",
            "ls \u{e0041}\u{e007f}",
        ] {
            let e = snippet(bad).render_checked(&[]).expect_err(bad);
            assert_eq!(code(&e), "snippet_invalid", "{bad:?}");
            assert!(snippet(bad).validate().is_err(), "{bad:?}");
            let e = snippet("echo {{v}}")
                .render_checked(&[("v", bad)])
                .expect_err(bad);
            assert_eq!(code(&e), "snippet_value", "{bad:?}");
            let named = Snippet {
                name: format!("n{bad}"),
                ..snippet("df")
            };
            assert!(named.validate().is_err(), "{bad:?}");
        }
        // The zero-width joiner of emoji sequences stays allowed.
        let family = "echo \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        snippet(family).validate().expect("zwj");
        assert_eq!(snippet(family).render_checked(&[]).expect("render"), family);
        snippet("echo {{v}}")
            .render_checked(&[("v", "\u{1f469}\u{200d}\u{1f4bb}")])
            .expect("zwj value");
    }

    #[test]
    fn hand_edited_file_with_duplicate_ids_and_bidi_name_is_sanitized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("snippets.toml");
        std::fs::write(
            &path,
            "[[snippet]]\nid = \"df\"\nname = \"Disk\"\ncommand = \"df -h\"\nscope = { kind = \"global\" }\n\n\
             [[snippet]]\nid = \"df\"\nname = \"Disk inodes\"\ncommand = \"df -i\"\nscope = { kind = \"global\" }\n\n\
             [[snippet]]\nid = \"Bad_Id\"\nname = \"Up\"\ncommand = \"uptime\"\nscope = { kind = \"global\" }\n\n\
             [[snippet]]\nid = \"evil\"\nname = \"safe\u{202e}txt.sh\"\ncommand = \"ls\"\nscope = { kind = \"global\" }\n\n\
             [[snippet]]\nid = \"esc\"\nname = \"Esc\"\ncommand = \"echo \\u001b[201~\"\nscope = { kind = \"global\" }\n",
        )
        .expect("write");
        let store = SnippetStore::new(&path);
        let list = store.list().expect("list");
        let ids: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
        // Duplicate id re-derived, malformed id re-derived, invalid entries
        // (bidi name, ESC in the command) hidden.
        assert_eq!(ids, ["df", "disk-inodes", "up"]);
        assert!(list.iter().all(|s| s.validate().is_ok()));
        // Ids are unique, so each one addresses exactly one snippet.
        store.delete("disk-inodes").expect("delete");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("df -h") && !text.contains("df -i"), "{text}");
        // The hidden entries are kept in the file, never shown.
        assert!(text.contains("id = \"evil\""), "{text}");
        let ids: Vec<String> = store
            .list()
            .expect("list")
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["df", "up"]);
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("snippets.toml");
        let size = usize::try_from(MAX_SNIPPETS_FILE_BYTES).expect("size") + 1;
        std::fs::write(&path, "#".repeat(size)).expect("write");
        let e = SnippetStore::new(&path).list().expect_err("too large");
        assert_eq!(code(&e), "snippet_file_too_large");
        assert!(e.summary().hint.is_some());
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Redémarrer nginx !"), "redemarrer-nginx");
        assert_eq!(slugify("  Disk usage (df -h)"), "disk-usage-df-h");
        assert_eq!(slugify("Œuvre"), "oeuvre");
        assert_eq!(slugify("日本"), "snippet");
        assert_eq!(slugify(&"a".repeat(100)).len(), MAX_SLUG_BASE);
        validate_id(&slugify(&format!("{}-x", "a".repeat(47)))).expect("valid slug");
    }

    #[test]
    fn crud_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SnippetStore::new(dir.path().join("config/snippets.toml"));
        assert!(store.list().expect("list").is_empty());

        let first = store
            .save(Snippet {
                name: " Disk usage ".into(),
                ..snippet("df -h\n")
            })
            .expect("save");
        assert_eq!(first.id, "disk-usage");
        assert_eq!(first.name, "Disk usage");
        assert_eq!(first.command, "df -h");
        let second = store
            .save(Snippet {
                name: "Disk usage".into(),
                ..snippet("df -i")
            })
            .expect("save");
        assert_eq!(second.id, "disk-usage-2");
        let third = store
            .save(Snippet {
                name: "Disk usage".into(),
                ..snippet("du -sh")
            })
            .expect("save");
        assert_eq!(third.id, "disk-usage-3");

        // Update by id keeps the position.
        let updated = store
            .save(Snippet {
                command: "df -hT".into(),
                ..first.clone()
            })
            .expect("update");
        let list = store.list().expect("list");
        assert_eq!(list.len(), 3);
        assert_eq!(list.first(), Some(&updated));

        // Invalid snippets never reach the file.
        let e = store.save(snippet("echo \u{1b}")).expect_err("invalid");
        assert_eq!(code(&e), "snippet_invalid");
        assert_eq!(store.list().expect("list").len(), 3);

        store.delete("disk-usage-2").expect("delete");
        let e = store.delete("disk-usage-2").expect_err("gone");
        assert_eq!(code(&e), "snippet_not_found");
        let ids: Vec<String> = store
            .list()
            .expect("list")
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["disk-usage", "disk-usage-3"]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn scope_matching() {
        let tags = vec!["Prod".to_owned()];
        assert!(snippet("x").applies_to_alias("db", &[]));
        let hosts = Snippet {
            scope: SnippetScope::Hosts(vec!["db".into()]),
            ..snippet("x")
        };
        assert!(hosts.applies_to_alias("db", &[]));
        assert!(!hosts.applies_to_alias("web", &tags));
        let tagged = Snippet {
            scope: SnippetScope::Tags(vec!["prod".into()]),
            ..snippet("x")
        };
        assert!(tagged.applies_to_alias("web", &tags));
        assert!(!tagged.applies_to_alias("web", &[]));
    }

    #[test]
    fn toml_roundtrip() {
        let file = SnippetFile {
            snippets: vec![
                Snippet {
                    id: "df".into(),
                    name: "Disk usage".into(),
                    command: "df -h\nuptime".into(),
                    description: Some("Human readable".into()),
                    scope: SnippetScope::Tags(vec!["prod".into()]),
                    run: false,
                },
                Snippet {
                    id: "logs".into(),
                    name: "Logs".into(),
                    command: "journalctl -u {{unit}}".into(),
                    description: None,
                    scope: SnippetScope::Global,
                    run: true,
                },
            ],
        };
        let text = toml::to_string_pretty(&file).unwrap();
        assert_eq!(toml::from_str::<SnippetFile>(&text).unwrap(), file);
        // Hand-written file: `run` defaults to true, `id` may be omitted.
        let hand: SnippetFile = toml::from_str(
            "[[snippet]]\nname = \"Up\"\ncommand = \"uptime\"\nscope = { kind = \"global\" }\n",
        )
        .unwrap();
        let s = hand.snippets.first().unwrap();
        assert!(s.run);
        assert!(s.id.is_empty());
    }

    #[test]
    fn hand_written_entries_get_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("snippets.toml");
        std::fs::write(
            &path,
            "[[snippet]]\nname = \"Up\"\ncommand = \"uptime\"\nscope = { kind = \"global\" }\n\n\
             [[snippet]]\nname = \"Up\"\ncommand = \"w\"\nscope = { kind = \"global\" }\n",
        )
        .expect("write");
        let store = SnippetStore::new(&path);
        let ids: Vec<String> = store
            .list()
            .expect("list")
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, ["up", "up-2"]);
        store.delete("up-2").expect("delete by derived id");
        assert_eq!(store.list().expect("list").len(), 1);
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("id = \"up\"")
        );
    }
}
