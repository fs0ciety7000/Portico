//! `portico import termius FILE [--prefix] [--apply]` (Phase 5).
//!
//! Without `--apply` the plan is printed and nothing is written. With it,
//! the import is written (see `portico_core::import`) and the report ends
//! with the `Include` line the user adds to `~/.ssh/config` themselves:
//! Portico never edits that file. Plain text on stdout (no TUI), errors on
//! stderr as `ErrorSummary` (message + hint), never a secret: the core
//! never puts one in a plan or report.

use std::fmt::Write as _;
use std::path::PathBuf;

use portico_core::import::{ImportNotice, ImportOptions, ImportPlan, ImportReport, PlannedHost};
use portico_core::{Engine, EngineConfig, ErrorSummary};

/// `import termius` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportArgs {
    pub(crate) file: PathBuf,
    pub(crate) prefix: bool,
    pub(crate) apply: bool,
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
            _ => return Err("import termius takes one file".into()),
        }
    }
    let file = file.ok_or("import termius needs a file: portico import termius FILE")?;
    Ok(ImportArgs {
        file,
        prefix,
        apply,
    })
}

/// Runs the import; the text for stdout, or the error for stderr.
pub(crate) async fn run(args: ImportArgs) -> Result<String, ErrorSummary> {
    let engine = Engine::new(EngineConfig::discover().map_err(|e| e.summary())?);
    let options = ImportOptions {
        prefix: args.prefix,
    };
    let plan = engine
        .plan_termius_import(args.file, options)
        .await
        .map_err(|e| e.summary())?;
    let mut text = plan_text(&plan);
    if !args.apply {
        return Ok(text);
    }
    let shown_skipped = plan.skipped.len();
    let report = engine.apply_import(plan).await.map_err(|e| e.summary())?;
    text.push('\n');
    text.push_str(&report_text(&report, shown_skipped));
    Ok(text)
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

fn notice_line(notice: &ImportNotice) -> String {
    let mut line = String::from("  ");
    if let Some(record) = notice.record {
        let _ = write!(line, "line {record}: ");
    }
    if let Some(label) = &notice.label {
        let _ = write!(line, "{label}: ");
    }
    line.push_str(&notice.message);
    line
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
pub(crate) fn plan_text(plan: &ImportPlan) -> String {
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
    section(&mut text, "Hosts", plan.hosts.iter().map(host_line));
    section(&mut text, "Skipped", plan.skipped.iter().map(notice_line));
    section(&mut text, "Warnings", plan.warnings.iter().map(notice_line));
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
    let _ = writeln!(
        text,
        "\nNothing written yet. Add --apply to write {}.",
        plan.target.display()
    );
    text
}

/// What the import did, and the Include line.
/// `shown_skipped`: notices already printed with the plan.
pub(crate) fn report_text(report: &ImportReport, shown_skipped: usize) -> String {
    let mut text = String::new();
    if report.unchanged {
        let _ = writeln!(
            text,
            "{} already holds these {} (not rewritten).",
            report.target.display(),
            plural(report.imported.len(), "host")
        );
    } else {
        let _ = writeln!(
            text,
            "Wrote {} to {}.",
            plural(report.imported.len(), "host"),
            report.target.display()
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
    if let Some(advice) = report.secrets.advice() {
        let _ = writeln!(text, "\n{advice}");
    }
    if report.include_active {
        let _ = writeln!(
            text,
            "\n~/.ssh/config already includes it: the hosts are in Portico."
        );
    } else {
        let _ = writeln!(
            text,
            "\nAdd this line at the top of ~/.ssh/config (Portico never edits that file):\n\n    {}",
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
        assert!(parse(&args(&["termius"])).is_err());
        assert!(parse(&args(&["termius", "a", "b"])).is_err());
        assert!(parse(&args(&["termius", "a", "--force"])).is_err());
    }

    #[tokio::test]
    async fn plan_and_report_texts() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(EngineConfig {
            paths: portico_core::store::Paths::rooted(dir.path()),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: std::sync::Arc::new(portico_core::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: portico_core::engine::DEFAULT_CONNECT_TIMEOUT,
            keepalive: portico_core::engine::DEFAULT_KEEPALIVE,
        });
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
        let text = plan_text(&plan);
        assert!(text.starts_with("Termius import: export.csv (Termius CSV)\n2 records: 1 host to import, 1 skipped.\n"), "{text}");
        assert!(
            text.contains(
                "  web-eu-1                 deploy@203.0.113.10:2222  @Prod/EU #web #web-serveur\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("line 3: switch: the protocol is not SSH\n"),
            "{text}"
        );
        assert!(text.contains("1 password and 0 keys ignored"), "{text}");
        assert!(text.contains("Nothing written yet. Add --apply"), "{text}");
        assert!(!text.contains("FAKEPASS123"));

        let report = engine.apply_import(plan).await.unwrap();
        let text = report_text(&report, 1);
        assert!(text.starts_with("Wrote 1 host to "), "{text}");
        assert!(
            text.ends_with("\n    Include ~/config/imports/termius.conf\n"),
            "{text}"
        );
        assert!(!text.contains("FAKEPASS123"));
    }
}
