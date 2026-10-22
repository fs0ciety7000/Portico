//! `portico import termius FILE [--prefix] [--apply]` (Phase 5).
//!
//! Without `--apply` the plan is printed and nothing is written. With it,
//! the import is written (see `portico_core::import`) and the report ends
//! with the `Include` line the user adds to `~/.ssh/config` themselves:
//! Portico never edits that file. A plan with no host to import is never
//! applied (the previous import is left as is). Plain text on stdout (no
//! TUI), errors on stderr as `ErrorSummary` (message + hint), never a
//! secret: the core never puts one in a plan or report.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use portico_core::import::{
    IgnoredSecrets, ImportNotice, ImportOptions, ImportPlan, ImportReport, NoticeCode, PlannedHost,
};
use portico_core::{Engine, EngineConfig, ErrorSummary};

/// `import termius` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportArgs {
    pub(crate) file: PathBuf,
    pub(crate) prefix: bool,
    pub(crate) apply: bool,
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

/// Parses what follows `import`; `Err` is a usage message.
pub(crate) fn parse(args: &[String]) -> Result<ImportArgs, String> {
    let Some((source, rest)) = args.split_first() else {
        return Err("import needs a source: portico import termius FILE".into());
    };
    if source != "termius" {
        return Err(format!(
            "unknown import source: {source} (supported: termius)"
        ));
    }
    let mut file = None;
    let (mut prefix, mut apply) = (false, false);
    for arg in rest {
        match arg.as_str() {
            "--prefix" => prefix = true,
            "--apply" => apply = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option: {flag}")),
            path if file.is_none() => file = Some(PathBuf::from(path)),
            _ => return Err("import termius takes one FILE".into()),
        }
    }
    let file = file.ok_or("import termius needs a FILE")?;
    Ok(ImportArgs {
        file,
        prefix,
        apply,
    })
}

/// Runs the import; the text for stdout, or what to print before the error.
pub(crate) async fn run(args: ImportArgs) -> Result<String, Failure> {
    let config = EngineConfig::discover().map_err(|e| e.summary())?;
    let home = config.paths.home.clone();
    run_with(&Engine::new(config), &home, args).await
}

async fn run_with(engine: &Engine, home: &Path, args: ImportArgs) -> Result<String, Failure> {
    let options = ImportOptions {
        prefix: args.prefix,
    };
    let plan = engine
        .plan_termius_import(args.file.clone(), options)
        .await
        .map_err(|e| e.summary())?;
    let mut text = plan_text(&plan, home, &args);
    if !args.apply {
        return Ok(text);
    }
    if plan.hosts.is_empty() {
        return Err(Failure {
            stdout: text,
            summary: nothing_to_import(),
        });
    }
    let shown_skipped = plan.skipped.len();
    let report = engine.apply_import(plan).await.map_err(|e| Failure {
        stdout: text.clone(),
        summary: e.summary(),
    })?;
    text.push('\n');
    text.push_str(&report_text(&report, shown_skipped, home));
    Ok(text)
}

fn nothing_to_import() -> ErrorSummary {
    ErrorSummary {
        code: "import_nothing".into(),
        message: "nothing to import: termius.conf left as is".into(),
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
    line
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

/// What the import would do.
pub(crate) fn plan_text(plan: &ImportPlan, home: &Path, args: &ImportArgs) -> String {
    let mut text = String::new();
    let name = plan
        .source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = writeln!(
        text,
        "Termius import: {} ({})",
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
            "Replaces the {} already in {target}.",
            plural(previous, "host")
        );
    }
    section(&mut text, "Hosts", plan.hosts.iter().map(host_line));
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
        let _ = writeln!(text, "\nEvery alias is prefixed with `termius-`.");
    }
    if !args.apply {
        if plan.hosts.is_empty() {
            let _ = writeln!(text, "\nNothing to import: {target} would be left as is.");
        } else {
            let prefix = if args.prefix { " --prefix" } else { "" };
            let _ = writeln!(
                text,
                "\nNothing written yet. Run again with --apply: portico import termius {}{prefix} --apply",
                shell_word(&args.file.display().to_string())
            );
        }
    }
    text
}

/// "Delete the export now: it holds 2 passwords and 1 key in clear text."
fn delete_advice(secrets: IgnoredSecrets) -> Option<String> {
    (secrets.total() > 0).then(|| {
        format!(
            "Delete the export now: it holds {} and {} in clear text.",
            plural(secrets.passwords, "password"),
            plural(secrets.keys, "key")
        )
    })
}

/// What the import did, and the Include line.
/// `shown_skipped`: notices already printed with the plan.
pub(crate) fn report_text(report: &ImportReport, shown_skipped: usize, home: &Path) -> String {
    let mut text = String::new();
    let target = shown_path(&report.target, home);
    if report.unchanged {
        let _ = writeln!(
            text,
            "{target} already holds these {} (not rewritten).",
            plural(report.imported.len(), "host")
        );
    } else {
        let _ = writeln!(
            text,
            "Wrote {} to {target}.",
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
        "Skipped while writing (changed since the plan)",
        report.skipped.iter().skip(shown_skipped).map(notice_line),
    );
    if let Some(advice) = delete_advice(report.secrets) {
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
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn engine(root: &Path) -> Engine {
        Engine::new(EngineConfig {
            paths: portico_core::store::Paths::rooted(root),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: std::sync::Arc::new(portico_core::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: portico_core::engine::DEFAULT_CONNECT_TIMEOUT,
            keepalive: portico_core::engine::DEFAULT_KEEPALIVE,
        })
    }

    #[test]
    fn arguments() {
        assert_eq!(
            parse(&args(&["termius", "export.csv"])),
            Ok(ImportArgs {
                file: "export.csv".into(),
                prefix: false,
                apply: false
            })
        );
        assert_eq!(
            parse(&args(&["termius", "--apply", "my export.csv", "--prefix"])),
            Ok(ImportArgs {
                file: "my export.csv".into(),
                prefix: true,
                apply: true
            })
        );
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["putty", "x"])).is_err());
        assert_eq!(
            parse(&args(&["termius"])),
            Err("import termius needs a FILE".into())
        );
        assert!(parse(&args(&["termius", "a", "b"])).is_err());
        assert!(parse(&args(&["termius", "a", "--force"])).is_err());
    }

    #[test]
    fn paths_and_shell_words() {
        let home = Path::new("/home/ana");
        assert_eq!(
            shown_path(
                Path::new("/home/ana/.config/portico/imports/termius.conf"),
                home
            ),
            "~/.config/portico/imports/termius.conf"
        );
        assert_eq!(shown_path(Path::new("/opt/x"), home), "/opt/x");
        assert_eq!(shell_word("~/Downloads/t.csv"), "~/Downloads/t.csv");
        assert_eq!(shell_word("my export.csv"), "'my export.csv'");
        assert_eq!(shell_word("it's.csv"), "'it'\\''s.csv'");
    }

    #[tokio::test]
    async fn plan_and_report_texts() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let csv = dir.path().join("export.csv");
        std::fs::write(
            &csv,
            "Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password,SSH_KEY\n\
             Prod/EU,web eu 1,\"web,Web Serveur\",203.0.113.10,ssh,2222,deploy,FAKEPASS123,\n\
             ,switch,,192.0.2.50,telnet,23,admin,,\n",
        )
        .unwrap();
        let plan = engine
            .plan_termius_import(&csv, ImportOptions::default())
            .await
            .unwrap();
        let cli = ImportArgs {
            file: "my export.csv".into(),
            prefix: false,
            apply: false,
        };
        let text = plan_text(&plan, dir.path(), &cli);
        assert!(text.starts_with("Termius import: export.csv (Termius CSV)\n2 records: 1 host to import, 1 skipped.\n\n"), "{text}");
        assert!(!text.contains("Replaces"), "{text}");
        assert!(
            text.contains(
                "  web-eu-1                 deploy@203.0.113.10:2222  @Prod/EU #web #web-serveur\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("  line 3 · switch — the protocol is not SSH\n"),
            "{text}"
        );
        assert!(text.contains("1 password and 0 keys ignored"), "{text}");
        assert!(
            text.ends_with(
                "Nothing written yet. Run again with --apply: portico import termius 'my export.csv' --apply\n"
            ),
            "{text}"
        );
        assert!(!text.contains("FAKEPASS123"));

        let report = engine.apply_import(plan).await.unwrap();
        let text = report_text(&report, 1, dir.path());
        assert!(
            text.starts_with("Wrote 1 host to ~/config/imports/termius.conf.\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "\nDelete the export now: it holds 1 password and 0 keys in clear text.\n"
            ),
            "{text}"
        );
        assert!(
            !text.contains("ignored (never imported)"),
            "advice printed once: {text}"
        );
        assert!(
            text.ends_with("\n    Include ~/config/imports/termius.conf\n"),
            "{text}"
        );
        assert!(!text.contains("FAKEPASS123"));

        // A second import says what it replaces.
        let plan = engine
            .plan_termius_import(&csv, ImportOptions::default())
            .await
            .unwrap();
        let text = plan_text(&plan, dir.path(), &cli);
        assert!(
            text.contains("\nReplaces the 1 host already in ~/config/imports/termius.conf.\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn nothing_to_import_is_never_applied() {
        let dir = tempfile::tempdir().unwrap();
        let engine = engine(dir.path());
        let csv = dir.path().join("switches.csv");
        std::fs::write(
            &csv,
            "Label,Hostname/IP,Protocol\nswitch,192.0.2.50,telnet\n",
        )
        .unwrap();
        let plan = engine
            .plan_termius_import(&csv, ImportOptions::default())
            .await
            .unwrap();
        let cli = ImportArgs {
            file: csv.clone(),
            prefix: false,
            apply: false,
        };
        let text = plan_text(&plan, dir.path(), &cli);
        assert!(
            text.ends_with(
                "\nNothing to import: ~/config/imports/termius.conf would be left as is.\n"
            ),
            "{text}"
        );
        let target = plan.target.clone();
        let failure = run_with(&engine, dir.path(), ImportArgs { apply: true, ..cli })
            .await
            .expect_err("refused");
        assert_eq!(
            failure.summary.message,
            "nothing to import: termius.conf left as is"
        );
        assert!(
            failure.stdout.contains("0 hosts to import, 1 skipped"),
            "{}",
            failure.stdout
        );
        assert!(!target.exists(), "never written");
    }

    #[test]
    fn identity_file_notices_are_not_repeated() {
        let plan = ImportPlan {
            source: "x".into(),
            format: portico_core::import::TermiusFormat::SshConfig,
            options: ImportOptions::default(),
            records: 1,
            hosts: Vec::new(),
            skipped: Vec::new(),
            warnings: vec![ImportNotice {
                code: NoticeCode::IdentityFileIgnored,
                record: Some(3),
                label: None,
                message: "IdentityFile not imported".into(),
            }],
            secrets: IgnoredSecrets::default(),
            identity_files: vec!["~/.termius/ssh_keys/db".into()],
            target: "/t/termius.conf".into(),
            include_line: "Include /t/termius.conf".into(),
            replaces: None,
        };
        let cli = ImportArgs {
            file: "x".into(),
            prefix: false,
            apply: false,
        };
        let text = plan_text(&plan, Path::new("/home/ana"), &cli);
        assert!(!text.contains("Warnings"), "{text}");
        assert!(
            text.contains("Identity files not imported\n  ~/.termius/ssh_keys/db\n"),
            "{text}"
        );
    }
}
