//! `portico import [termius|termix] FILE [--prefix] [--key SHA256:…]…
//! [--all-keys] [--key-passphrase] [--apply]` (phases 5 and 6b).
//!
//! The format (Termius or Termix) is detected from the file's content;
//! `termius` / `termix` before the file are kept as aliases (`termix`
//! refuses any other format). Without `--apply` the plan is printed and
//! nothing is written. With it, the hosts become Portico hosts in
//! `hosts.conf` (see `portico_core::import`) and the report ends with the
//! `Include` line the user adds to `~/.ssh/config` themselves: Portico
//! never edits that file. A plan with no host to import is never applied.
//!
//! Private keys of an export are listed (type, fingerprint, hosts using
//! them, encrypted or not, local copy reused) and **none is imported
//! unless selected** (`--key`, repeatable, or `--all-keys`). The passphrase
//! that encrypts the copies is never a command-line value (arguments are
//! visible in `ps`): `--key-passphrase` asks for it on the terminal, echo
//! off, twice; it lives in `Zeroizing` / `SecretString` only. Plain text
//! on stdout (no TUI), errors on stderr as `ErrorSummary` (message + hint),
//! never a secret: the core never puts one in a plan or report.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use portico_core::import::{
    ImportNotice, ImportOptions, ImportPlan, ImportReport, KeyFormat, KeyId, KeySelection,
    NoticeCode, PlannedHost, PlannedKey,
};
use portico_core::{Engine, EngineConfig, ErrorSummary};
use secrecy::SecretString;
use zeroize::Zeroizing;

/// Passphrase attempts before giving up (mismatch or empty).
const PASSPHRASE_ATTEMPTS: usize = 3;
/// Key fingerprints are `SHA256:` then base64 (43 characters).
const FINGERPRINT_PREFIX: &str = "SHA256:";

/// The word after `import`, when one was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceArg {
    /// `import FILE`: Termius or Termix, from the content.
    Detect,
    /// `import termius FILE`: the historical form (every format detected).
    Termius,
    /// `import termix FILE`: a Termix export only.
    Termix,
}

impl SourceArg {
    /// The word to repeat in a suggested command (`termius `), or nothing.
    fn word(self) -> &'static str {
        match self {
            Self::Detect => "",
            Self::Termius => "termius ",
            Self::Termix => "termix ",
        }
    }
}

/// `import` arguments.
// Independent command-line flags, not one state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportArgs {
    pub(crate) source: SourceArg,
    pub(crate) file: PathBuf,
    pub(crate) prefix: bool,
    pub(crate) apply: bool,
    /// `--key` fingerprints, in the order given (duplicates removed).
    pub(crate) keys: Vec<String>,
    /// `--all-keys`: every key of the plan.
    pub(crate) all_keys: bool,
    /// `--key-passphrase`: ask on the terminal for a passphrase that
    /// encrypts the written copies (never a value on the command line).
    pub(crate) key_passphrase: bool,
}

/// A failed run: what was already printable (the plan), then the error.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) stdout: String,
    pub(crate) summary: ErrorSummary,
}

impl From<ErrorSummary> for Failure {
    fn from(summary: ErrorSummary) -> Self {
        Self {
            stdout: String::new(),
            summary,
        }
    }
}

const PASSPHRASE_AS_VALUE: &str = "--key-passphrase takes no value: Portico asks for the passphrase on the terminal (command-line arguments are visible to other users in `ps`)";

/// Parses what follows `import`; `Err` is a usage message. A message
/// never repeats a value given to `--key-passphrase` or after `=`.
pub(crate) fn parse(args: &[String]) -> Result<ImportArgs, String> {
    let (source, rest) = match args.split_first() {
        None => return Err("import needs a file: portico import FILE".into()),
        Some((first, [])) if first == "termius" || first == "termix" => {
            return Err(format!("import {first} needs a FILE"));
        }
        Some((first, rest)) if first == "termius" && !rest.is_empty() => (SourceArg::Termius, rest),
        Some((first, rest)) if first == "termix" && !rest.is_empty() => (SourceArg::Termix, rest),
        Some(_) => (SourceArg::Detect, args),
    };
    let mut file = None;
    let (mut prefix, mut apply, mut all_keys, mut key_passphrase) = (false, false, false, false);
    let mut keys: Vec<String> = Vec::new();
    let mut previous_was_passphrase = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let after_passphrase = std::mem::take(&mut previous_was_passphrase);
        match arg.as_str() {
            "--prefix" => prefix = true,
            "--apply" => apply = true,
            "--all-keys" => all_keys = true,
            "--key-passphrase" => {
                key_passphrase = true;
                previous_was_passphrase = true;
            }
            "--key" => {
                let id = iter
                    .next()
                    .ok_or("--key needs a fingerprint: --key SHA256:…")?;
                push_key(&mut keys, id)?;
            }
            flag if flag.starts_with("--key-passphrase=") => {
                return Err(PASSPHRASE_AS_VALUE.into());
            }
            flag if flag.starts_with("--key=") => {
                push_key(&mut keys, flag.trim_start_matches("--key="))?;
            }
            flag if flag.starts_with("--") => {
                // Only the name: a value after `=` could be a secret.
                let name = flag.split('=').next().unwrap_or(flag);
                return Err(format!("unknown option: {name}"));
            }
            path if file.is_none() => file = Some(PathBuf::from(path)),
            _ if after_passphrase => return Err(PASSPHRASE_AS_VALUE.into()),
            _ => return Err("import takes one FILE".into()),
        }
    }
    let file = file.ok_or("import needs a FILE")?;
    Ok(ImportArgs {
        source,
        file,
        prefix,
        apply,
        keys,
        all_keys,
        key_passphrase,
    })
}

/// Adds a `--key` value: `SHA256:` then base64 (never echoed back).
fn push_key(keys: &mut Vec<String>, id: &str) -> Result<(), String> {
    let body = id.strip_prefix(FINGERPRINT_PREFIX).unwrap_or_default();
    let valid = !body.is_empty()
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    if !valid {
        return Err("--key needs a key fingerprint as listed in the plan: --key SHA256:…".into());
    }
    if !keys.iter().any(|k| k == id) {
        keys.push(id.to_owned());
    }
    Ok(())
}

/// Reads a passphrase from the user (the terminal, or a test double).
pub(crate) trait SecretPrompt: Send + 'static {
    /// Shows `prompt`, reads one line without echo.
    fn read(&mut self, prompt: &str) -> io::Result<Zeroizing<String>>;
    /// Shows a short message (mismatch, empty).
    fn note(&mut self, message: &str);
}

/// The controlling terminal, read in raw mode (no echo).
#[derive(Debug, Default)]
pub(crate) struct TtyPrompt;

impl SecretPrompt for TtyPrompt {
    fn read(&mut self, prompt: &str) -> io::Result<Zeroizing<String>> {
        read_secret_from_tty(prompt)
    }

    fn note(&mut self, message: &str) {
        let _ = tty_write(&format!("{message}\r\n"));
    }
}

/// Writes to the terminal itself, so stdout can be piped.
fn tty_write(text: &str) -> io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    {
        let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
        tty.write_all(text.as_bytes())?;
        tty.flush()
    }
    #[cfg(not(unix))]
    {
        let mut err = io::stderr();
        err.write_all(text.as_bytes())?;
        err.flush()
    }
}

/// Leaves raw mode on every path out of [`read_secret_from_tty`].
struct RawMode;

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// One line typed on the terminal, echo off (raw mode: nothing is
/// printed, not even a mask, so the length does not show). `Ctrl-C` /
/// `Esc` abort.
fn read_secret_from_tty(prompt: &str) -> io::Result<Zeroizing<String>> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    tty_write(prompt)?;
    crossterm::terminal::enable_raw_mode()?;
    let raw = RawMode;
    let mut line = Zeroizing::new(String::new());
    let outcome = loop {
        let Event::Key(key) = read()? else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => break Ok(()),
            KeyCode::Char('c' | 'd') if ctrl => break Err(io::ErrorKind::Interrupted),
            KeyCode::Esc => break Err(io::ErrorKind::Interrupted),
            KeyCode::Char('u') if ctrl => line.clear(),
            KeyCode::Backspace => {
                line.pop();
            }
            KeyCode::Char(c) if !ctrl => line.push(c),
            _ => {}
        }
    };
    drop(raw);
    let _ = tty_write("\r\n");
    outcome.map_err(io::Error::from)?;
    Ok(line)
}

/// Asks for the passphrase twice; `Err` when the user gave up.
fn ask_passphrase(prompt: &mut dyn SecretPrompt) -> Result<SecretString, ErrorSummary> {
    let failed = |message: &str| ErrorSummary {
        code: "import_passphrase".into(),
        message: message.into(),
        hint: Some(
            "Run again, or leave out --key-passphrase to write the keys unencrypted (0600).".into(),
        ),
    };
    for _ in 0..PASSPHRASE_ATTEMPTS {
        let first = prompt
            .read("Passphrase for the imported keys: ")
            .map_err(|e| failed(&passphrase_io_message(&e)))?;
        if first.is_empty() {
            prompt.note("The passphrase is empty: type one, or press Ctrl-C to stop.");
            continue;
        }
        let second = prompt
            .read("Same passphrase again: ")
            .map_err(|e| failed(&passphrase_io_message(&e)))?;
        if *first == *second {
            return Ok(SecretString::from(first.as_str()));
        }
        prompt.note("The two passphrases differ: try again.");
    }
    Err(failed("no passphrase confirmed: nothing was written"))
}

fn passphrase_io_message(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::Interrupted {
        "passphrase not given: nothing was written".into()
    } else {
        format!("cannot read the passphrase from the terminal: {error}")
    }
}

/// Runs the import; the text for stdout, or what to print before the error.
pub(crate) async fn run(args: ImportArgs) -> Result<String, Failure> {
    let config = EngineConfig::discover().map_err(|e| e.summary())?;
    let home = config.paths.home.clone();
    run_with(&Engine::new(config), &home, args, TtyPrompt).await
}

async fn plan(engine: &Engine, args: &ImportArgs) -> Result<ImportPlan, ErrorSummary> {
    let options = ImportOptions {
        prefix: args.prefix,
        ..ImportOptions::default()
    };
    let file = args.file.clone();
    match args.source {
        SourceArg::Termix => engine.plan_termix_import(file, options).await,
        SourceArg::Termius => engine.plan_termius_import(file, options).await,
        SourceArg::Detect => engine.plan_import(file, options).await,
    }
    .map_err(|e| e.summary())
}

/// The keys `args` select in `plan`, or the first `--key` it lacks.
fn selection(plan: &ImportPlan, args: &ImportArgs) -> Result<KeySelection, String> {
    if args.all_keys {
        return Ok(if plan.keys.is_empty() {
            KeySelection::None
        } else {
            KeySelection::Some(plan.keys.iter().map(|k| k.id.clone()).collect())
        });
    }
    if args.keys.is_empty() {
        return Ok(KeySelection::None);
    }
    let mut ids = BTreeSet::new();
    for wanted in &args.keys {
        let id = KeyId::new(wanted.as_str());
        if !plan.keys.iter().any(|k| k.id == id) {
            return Err(wanted.clone());
        }
        ids.insert(id);
    }
    Ok(KeySelection::Some(ids))
}

/// Keys the passphrase would encrypt: selected, not reused, not already
/// encrypted in the export.
fn keys_to_encrypt(plan: &ImportPlan, selected: &KeySelection) -> usize {
    plan.keys
        .iter()
        .filter(|k| selected.contains(&k.id) && k.local.is_none() && !k.encrypted)
        .count()
}

pub(crate) async fn run_with<P: SecretPrompt>(
    engine: &Engine,
    home: &Path,
    args: ImportArgs,
    mut prompt: P,
) -> Result<String, Failure> {
    let plan = plan(engine, &args).await?;
    let selected = match selection(&plan, &args) {
        Ok(selected) => selected,
        Err(missing) => {
            return Err(Failure {
                stdout: plan_text(&plan, home, &args, &KeySelection::None),
                summary: ErrorSummary {
                    code: "import_unknown_key".into(),
                    message: format!("no key {missing} in this export"),
                    hint: Some(
                        "Pick a fingerprint from \"Private keys\" above, or use --all-keys.".into(),
                    ),
                },
            });
        }
    };
    let mut text = plan_text(&plan, home, &args, &selected);
    if !args.apply {
        return Ok(text);
    }
    if plan.hosts.is_empty() {
        return Err(Failure {
            stdout: text,
            summary: nothing_to_import(),
        });
    }
    let key_passphrase = if args.key_passphrase && keys_to_encrypt(&plan, &selected) > 0 {
        let asked = tokio::task::spawn_blocking(move || ask_passphrase(&mut prompt))
            .await
            .map_err(|_| ErrorSummary {
                code: "import_passphrase".into(),
                message: "the passphrase prompt stopped: nothing was written".into(),
                hint: None,
            });
        match asked.and_then(|r| r) {
            Ok(secret) => Some(secret),
            Err(summary) => {
                return Err(Failure {
                    stdout: text,
                    summary,
                });
            }
        }
    } else {
        None
    };
    let shown = Shown {
        skipped: plan.skipped.clone(),
        warnings: plan.warnings.clone(),
        encrypted: key_passphrase.is_some(),
    };
    let options = ImportOptions {
        prefix: args.prefix,
        keys: selected,
        key_passphrase,
    };
    let report = engine
        .apply_import(plan, options)
        .await
        .map_err(|e| Failure {
            stdout: text.clone(),
            summary: e.summary(),
        })?;
    text.push('\n');
    text.push_str(&report_text(&report, &shown, home));
    Ok(text)
}

fn nothing_to_import() -> ErrorSummary {
    ErrorSummary {
        code: "import_nothing".into(),
        message: "nothing to import: hosts.conf left as is".into(),
        hint: Some("Every record was skipped (see above). Try --prefix or another export.".into()),
    }
}

/// `path` with the home directory shown as `~`.
fn shown_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// A shell word: single-quoted when it has anything but safe characters.
fn shell_word(text: &str) -> String {
    let safe = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~' | '+' | ':' | '@')
        });
    if safe {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn host_line(host: &PlannedHost) -> String {
    let user = host
        .user
        .as_ref()
        .map(|u| format!("{u}@"))
        .unwrap_or_default();
    let port = host.port.map(|p| format!(":{p}")).unwrap_or_default();
    let mut line = format!("  {:<24} {user}{}{port}", host.alias, host.hostname);
    if let Some(group) = &host.group {
        let _ = write!(line, "  @{group}");
    }
    for tag in &host.tags {
        let _ = write!(line, " #{tag}");
    }
    if host.update {
        line.push_str("  (update)");
    }
    line
}

fn format_label(format: KeyFormat) -> &'static str {
    match format {
        KeyFormat::OpenSsh => "OpenSSH",
        KeyFormat::Pem => "PEM",
        _ => "other format",
    }
}

/// Two lines per key: what it is, then what it is for.
fn key_lines(key: &PlannedKey, selected: &KeySelection, home: &Path) -> [String; 2] {
    let mark = if selected.contains(&key.id) {
        "  [selected]"
    } else {
        ""
    };
    let first = format!("  {:<20} {}{mark}", key.algorithm, key.fingerprint);
    let mut second = format!(
        "    {}, {}",
        format_label(key.format),
        if key.encrypted {
            "encrypted"
        } else {
            "not encrypted"
        }
    );
    if key.aliases.is_empty() {
        second.push_str(" · used by no imported host");
    } else {
        let _ = write!(second, " · used by {}", key.aliases.join(", "));
    }
    if let Some(local) = &key.local {
        let _ = write!(second, " · already in {} (reused)", shown_path(local, home));
    }
    [first, second]
}

/// `  line 5 · web eu 1 — message` (the GUI's separators).
fn notice_line(notice: &ImportNotice) -> String {
    let place: Vec<String> = notice
        .record
        .map(|r| format!("line {r}"))
        .into_iter()
        .chain(notice.label.clone())
        .collect();
    if place.is_empty() {
        format!("  {}", notice.message)
    } else {
        format!("  {} — {}", place.join(" · "), notice.message)
    }
}

fn section(text: &mut String, title: &str, lines: impl IntoIterator<Item = String>) {
    let lines: Vec<String> = lines.into_iter().collect();
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(text, "\n{title}");
    for line in lines {
        let _ = writeln!(text, "{line}");
    }
}

/// The command that applies this plan, as typed again.
fn apply_command(args: &ImportArgs) -> String {
    let mut command = format!(
        "portico import {}{}",
        args.source.word(),
        shell_word(&args.file.display().to_string())
    );
    if args.prefix {
        command.push_str(" --prefix");
    }
    if args.all_keys {
        command.push_str(" --all-keys");
    } else {
        for key in &args.keys {
            let _ = write!(command, " --key {}", shell_word(key));
        }
    }
    if args.key_passphrase {
        command.push_str(" --key-passphrase");
    }
    command.push_str(" --apply");
    command
}

/// What the import would do, with the keys `selected`.
pub(crate) fn plan_text(
    plan: &ImportPlan,
    home: &Path,
    args: &ImportArgs,
    selected: &KeySelection,
) -> String {
    let mut text = String::new();
    let name = plan
        .source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let source = plan.source_kind();
    let _ = writeln!(
        text,
        "{} import: {} ({})",
        source.label(),
        name.chars().filter(|c| !c.is_control()).collect::<String>(),
        plan.format.label()
    );
    let _ = writeln!(
        text,
        "{} records: {} to import, {} skipped.",
        plan.records,
        plural(plan.hosts.len(), "host"),
        plan.skipped.len()
    );
    let target = shown_path(&plan.target, home);
    if let Some(previous) = plan.replaces {
        let _ = writeln!(
            text,
            "Updates {} from an earlier import.",
            plural(previous, "host")
        );
    }
    section(&mut text, "Hosts", plan.hosts.iter().map(host_line));
    keys_section(&mut text, plan, args, selected, home);
    section(&mut text, "Skipped", plan.skipped.iter().map(notice_line));
    // IdentityFile notices repeat the dedicated list below.
    let listed = !plan.identity_files.is_empty();
    section(
        &mut text,
        "Warnings",
        plan.warnings
            .iter()
            .filter(|n| !(listed && n.code == NoticeCode::IdentityFileIgnored))
            .map(notice_line),
    );
    section(
        &mut text,
        "Secrets",
        plan.secrets.advice().map(|a| format!("  {a}")),
    );
    section(
        &mut text,
        "Identity files not imported",
        plan.identity_files.iter().map(|f| format!("  {f}")),
    );
    if plan.options.prefix {
        let _ = writeln!(
            text,
            "\nEvery alias is prefixed with `{}`.",
            source.prefix()
        );
    }
    if !args.apply {
        if plan.hosts.is_empty() {
            let _ = writeln!(text, "\nNothing to import: {target} would be left as is.");
        } else {
            let _ = writeln!(
                text,
                "\nNothing written yet. Run again with --apply: {}",
                apply_command(args)
            );
        }
    }
    text
}

fn keys_section(
    text: &mut String,
    plan: &ImportPlan,
    args: &ImportArgs,
    selected: &KeySelection,
    home: &Path,
) {
    if plan.keys.is_empty() {
        return;
    }
    let count = plan
        .keys
        .iter()
        .filter(|k| selected.contains(&k.id))
        .count();
    let _ = writeln!(
        text,
        "\nPrivate keys: {} found, {count} selected (none is imported unless selected)",
        plan.keys.len()
    );
    for key in &plan.keys {
        for line in key_lines(key, selected, home) {
            let _ = writeln!(text, "{line}");
        }
    }
    if count == 0 {
        let _ = writeln!(
            text,
            "  Select keys with --key SHA256:… (repeatable) or --all-keys. Selected keys go to ~/.ssh/portico (0600), or a local copy is reused."
        );
    } else if !args.key_passphrase && keys_to_encrypt(plan, selected) > 0 {
        let _ = writeln!(
            text,
            "  Selected keys not encrypted in the export are written without a passphrase (0600): add --key-passphrase to encrypt them."
        );
    }
}

/// What the plan already printed, and how the keys were written.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    pub(crate) skipped: Vec<ImportNotice>,
    pub(crate) warnings: Vec<ImportNotice>,
    /// A passphrase was given for the written keys.
    pub(crate) encrypted: bool,
}

/// "Delete the export now: it holds 2 passwords and 3 private keys."
fn delete_advice(report: &ImportReport) -> Option<String> {
    let secrets = report.secrets;
    // Termix counts the keys of the export (as the plan's advice does);
    // Termius only the key cells it ignored.
    let keys = if secrets.keys_in_export > 0 {
        secrets.keys_in_export
    } else {
        secrets.keys
    };
    (secrets.passwords.saturating_add(keys) > 0).then(|| {
        format!(
            "Delete the export now: it holds {} and {} (in clear text or encrypted). Under Linux: shred -u FILE.",
            plural(secrets.passwords, "password"),
            plural(keys, "private key")
        )
    })
}

/// What the import did, and the Include line.
pub(crate) fn report_text(report: &ImportReport, shown: &Shown, home: &Path) -> String {
    let mut text = String::new();
    let target = shown_path(&report.target, home);
    let updated = if report.updated.is_empty() {
        String::new()
    } else {
        format!(", {} updated from an earlier import", report.updated.len())
    };
    if report.unchanged {
        let _ = writeln!(
            text,
            "{target} already holds these {} (not rewritten).",
            plural(report.imported.len(), "host")
        );
    } else {
        let _ = writeln!(
            text,
            "Wrote {} to {target}{updated}.",
            plural(report.imported.len(), "host"),
        );
    }
    let _ = writeln!(
        text,
        "Metadata (group, tags, notes) updated for {} in hosts.toml.",
        plural(report.meta_updated, "host")
    );
    section(
        &mut text,
        "Updated hosts",
        report.updated.iter().map(|alias| format!("  {alias}")),
    );
    let keys = &report.keys;
    let imported_title = if shown.encrypted {
        "Keys imported (0600; those not encrypted in the export now use your passphrase)"
    } else {
        "Keys imported (0600)"
    };
    section(
        &mut text,
        imported_title,
        keys.imported
            .iter()
            .map(|p| format!("  {}", shown_path(p, home))),
    );
    section(
        &mut text,
        "Keys reused (already on this machine)",
        keys.reused
            .iter()
            .map(|p| format!("  {}", shown_path(p, home))),
    );
    if keys.skipped > 0 {
        let _ = writeln!(
            text,
            "\n{} not imported (not selected, or none of its hosts was imported).",
            plural(keys.skipped, "key")
        );
    }
    section(
        &mut text,
        "Skipped while writing (changed since the plan)",
        report
            .skipped
            .iter()
            .filter(|n| !shown.skipped.contains(n))
            .map(notice_line),
    );
    section(
        &mut text,
        "Warnings while writing",
        report
            .warnings
            .iter()
            .filter(|n| !shown.warnings.contains(n))
            .map(notice_line),
    );
    if let Some(advice) = delete_advice(report) {
        let _ = writeln!(text, "\n{advice}");
    }
    if report.include_active {
        let _ = writeln!(
            text,
            "\n~/.ssh/config already includes this file: the hosts are in your list."
        );
    } else {
        let _ = writeln!(
            text,
            "\nOne step left: add this line at the top of ~/.ssh/config (Portico never edits that file):\n\n    {}",
            report.include_line
        );
    }
    text
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

#[cfg(test)]
mod tests;
