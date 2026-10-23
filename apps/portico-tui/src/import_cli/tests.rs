//! `portico import`: argument parsing (key ids, `--all-keys`, a passphrase
//! never taken as a value), plan and report texts with keys, and runs on
//! the core's Termix fixture in a temporary home (never the real `~/.ssh`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;

const ED25519: &str = "SHA256:fAtep8ghWl7Uqy2Puv62F3OqOuzAcVZ7KpQw2C21KOM";
const LOCKED: &str = "SHA256:tFX16mEH5d2m/QSoYLOE37Kjmb0YVisJXVRPcL8CmDs";
const RSA: &str = "SHA256:6ktWbDQJePrgzl71bBw1B5CFNzBRJcqP6BOcMw0/Ycw";
/// Strings of the fixture that must never be printed.
const SECRETS: [&str; 4] = [
    "FAKEPASS123",
    "FAKEKEYPASS",
    "PRIVATE KEY",
    "FAKEKEYMATERIAL",
];

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn cli(file: impl Into<PathBuf>) -> ImportArgs {
    ImportArgs {
        source: SourceArg::Detect,
        file: file.into(),
        prefix: false,
        apply: false,
        keys: Vec::new(),
        all_keys: false,
        key_passphrase: false,
        accept_redirect: Vec::new(),
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/portico-core/tests/fixtures/termix/termix_export.json")
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
        update_fetcher: portico_core::update::no_network(),
    })
}

fn assert_no_secret(text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

/// Answers prompts from a script; records prompts and notes.
#[derive(Default, Clone)]
struct Scripted {
    answers: Arc<Mutex<VecDeque<io::Result<String>>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Scripted {
    fn new(answers: impl IntoIterator<Item = io::Result<&'static str>>) -> Self {
        let answers = answers.into_iter().map(|a| a.map(str::to_owned)).collect();
        Self {
            answers: Arc::new(Mutex::new(answers)),
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl SecretPrompt for Scripted {
    fn read(&mut self, prompt: &str) -> io::Result<Zeroizing<String>> {
        self.asked.lock().unwrap().push(prompt.to_owned());
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::other("no scripted answer")))
            .map(Zeroizing::new)
    }

    fn note(&mut self, message: &str) {
        self.asked.lock().unwrap().push(format!("note: {message}"));
    }

    fn show(&mut self, text: &str) {
        self.asked.lock().unwrap().push(format!("show: {text}"));
    }
}

#[test]
fn arguments() {
    assert_eq!(parse(&args(&["export.csv"])), Ok(cli("export.csv")));
    assert_eq!(
        parse(&args(&["termius", "export.csv"])),
        Ok(ImportArgs {
            source: SourceArg::Termius,
            ..cli("export.csv")
        })
    );
    assert_eq!(
        parse(&args(&["termix", "--apply", "my export.json", "--prefix"])),
        Ok(ImportArgs {
            source: SourceArg::Termix,
            prefix: true,
            apply: true,
            ..cli("my export.json")
        })
    );
    // Keys: repeatable, `=` form, duplicates removed, --all-keys.
    assert_eq!(
        parse(&args(&[
            "x.json",
            "--key",
            ED25519,
            &format!("--key={RSA}"),
            "--key",
            ED25519,
            "--all-keys",
            "--key-passphrase",
        ])),
        Ok(ImportArgs {
            keys: vec![ED25519.to_owned(), RSA.to_owned()],
            all_keys: true,
            key_passphrase: true,
            ..cli("x.json")
        })
    );
    assert_eq!(
        parse(&args(&["--key-passphrase", "x.json"])),
        Ok(ImportArgs {
            key_passphrase: true,
            ..cli("x.json")
        }),
        "the flag before the file"
    );
    assert!(parse(&args(&["x.json", "--key"])).is_err(), "missing id");
    let bad = parse(&args(&["x.json", "--key", "hunter2"])).unwrap_err();
    assert!(!bad.contains("hunter2"), "{bad}");
    assert!(parse(&args(&["x.json", "--key", "SHA256:"])).is_err());
    assert!(parse(&args(&["x.json", "--key", "SHA256:a b"])).is_err());

    // A passphrase is never a value, and never echoed.
    for list in [
        &["x.json", "--key-passphrase=hunter2"][..],
        &["x.json", "--key-passphrase", "hunter2"][..],
        &["--key-passphrase", "x.json", "--key-passphrase", "hunter2"][..],
    ] {
        let message = parse(&args(list)).unwrap_err();
        assert!(message.contains("takes no value"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }
    let message = parse(&args(&["x.json", "--passphrase=hunter2"])).unwrap_err();
    assert_eq!(message, "unknown option: --passphrase");

    assert!(parse(&args(&[])).is_err());
    assert!(parse(&args(&["putty", "x"])).is_err());
    assert_eq!(
        parse(&args(&["termius"])),
        Err("import termius needs a FILE".into())
    );
    assert_eq!(
        parse(&args(&["termix"])),
        Err("import termix needs a FILE".into())
    );
    assert!(parse(&args(&["termius", "a", "b"])).is_err());
    assert!(parse(&args(&["termius", "a", "--force"])).is_err());
}

#[test]
fn accept_redirect_arguments() {
    // Repeatable, `=` form, lowercased, duplicates removed.
    assert_eq!(
        parse(&args(&[
            "x.json",
            "--accept-redirect",
            "Web-Prod",
            "--accept-redirect=db.eu_1",
            "--accept-redirect",
            "web-prod",
        ])),
        Ok(ImportArgs {
            accept_redirect: vec!["web-prod".to_owned(), "db.eu_1".to_owned()],
            ..cli("x.json")
        })
    );
    assert!(parse(&args(&["x.json", "--accept-redirect"])).is_err());
    // Outside the alias rules: refused, never echoed.
    for hostile in [
        "evil\nProxyCommand x",
        "\u{1b}[2Jwipe",
        "a b",
        "",
        "web/prod",
        &"a".repeat(MAX_ALIAS_CHARS + 1),
    ] {
        for list in [
            vec![
                "x.json".to_owned(),
                "--accept-redirect".to_owned(),
                hostile.to_owned(),
            ],
            vec!["x.json".to_owned(), format!("--accept-redirect={hostile}")],
        ] {
            let message = parse(&list).unwrap_err();
            assert_eq!(message, ACCEPT_REDIRECT_USAGE);
            if !hostile.is_empty() {
                assert!(!message.contains(hostile), "{message}");
            }
        }
    }
}

#[test]
fn paths_and_shell_words() {
    let home = Path::new("/home/ana");
    assert_eq!(
        shown_path(Path::new("/home/ana/.config/portico/hosts.conf"), home),
        "~/.config/portico/hosts.conf"
    );
    assert_eq!(shown_path(Path::new("/opt/x"), home), "/opt/x");
    assert_eq!(shell_word("~/Downloads/t.csv"), "~/Downloads/t.csv");
    assert_eq!(shell_word("my export.csv"), "'my export.csv'");
    assert_eq!(shell_word("it's.csv"), "'it'\\''s.csv'");
    assert_eq!(shell_word(LOCKED), LOCKED);
}

#[tokio::test]
async fn termius_plan_and_report_texts() {
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
    let args = ImportArgs {
        source: SourceArg::Termius,
        ..cli("my export.csv")
    };
    let text = plan_text(&plan, dir.path(), &args, &KeySelection::None);
    assert!(
        text.starts_with(
            "Termius import: export.csv (Termius CSV)\n2 records: 1 host to import, 1 skipped.\n\n"
        ),
        "{text}"
    );
    assert!(!text.contains("Updates"), "{text}");
    assert!(!text.contains("Private keys"), "{text}");
    assert!(
        text.contains("  web-eu-1 deploy@203.0.113.10:2222  @Prod/EU #web #web-serveur\n"),
        "{text}"
    );
    assert!(
        text.contains("  line 3 · switch — the protocol is not SSH\n"),
        "{text}"
    );
    assert!(
        text.contains("Secrets\n  Not imported: 1 password.\n"),
        "{text}"
    );
    assert!(!text.contains("0 keys"), "{text}");
    assert!(
        text.ends_with(
            "Nothing written yet. Run again with --apply: portico import termius 'my export.csv' --apply\n"
        ),
        "{text}"
    );
    assert_no_secret(&text);

    let shown = Shown::of_plan(&plan, dir.path(), &args);
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    let text = report_text(&report, &shown, dir.path());
    // The action on the export first, right after the summary, naming it.
    assert!(
        text.starts_with(
            "Wrote 1 host to ~/config/hosts.conf.\n▲ Delete export.csv now: it holds 1 password (in clear text or encrypted).\nMetadata (group, tags, notes) updated for 1 host in hosts.toml.\n"
        ),
        "{text}"
    );
    assert!(!text.contains("Under Linux"), "{text}");
    assert!(!text.contains("Skipped while writing"), "{text}");
    assert!(!text.contains("Keys"), "{text}");
    // The Include line, then the short version of the action, last.
    assert!(
        text.ends_with("\n    Include ~/config/hosts.conf\n\n▲ Delete export.csv now.\n"),
        "{text}"
    );
    assert_no_secret(&text);

    // A second import says what it updates, and its report names them.
    let plan = engine
        .plan_import(&csv, ImportOptions::default())
        .await
        .unwrap();
    let text = plan_text(&plan, dir.path(), &args, &KeySelection::None);
    assert!(
        text.contains("\nUpdates 1 host from an earlier import.\n"),
        "{text}"
    );
    assert!(text.contains(" #web-serveur  (update)\n"), "{text}");
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    let text = report_text(&report, &Shown::default(), dir.path());
    assert!(
        text.starts_with("~/config/hosts.conf already holds these 1 host (not rewritten).\n")
            || text.contains("1 updated from an earlier import"),
        "{text}"
    );
    assert!(text.contains("\nUpdated hosts\n  web-eu-1\n"), "{text}");
    // No metadata changed: no line saying "0 hosts".
    assert_eq!(report.meta_updated, 0);
    assert!(!text.contains("Metadata"), "{text}");
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
    let text = plan_text(&plan, dir.path(), &cli(&csv), &KeySelection::None);
    assert!(
        text.ends_with("\nNothing to import: ~/config/hosts.conf would be left as is.\n"),
        "{text}"
    );
    let target = plan.target.clone();
    let failure = run_with(
        &engine,
        dir.path(),
        ImportArgs {
            apply: true,
            ..cli(&csv)
        },
        Scripted::default(),
    )
    .await
    .expect_err("refused");
    assert_eq!(
        failure.summary.message,
        "nothing to import: hosts.conf left as is"
    );
    assert!(
        failure.stdout.contains("0 hosts to import, 1 skipped"),
        "{}",
        failure.stdout
    );
    assert!(!target.exists(), "never written");
}

#[tokio::test]
async fn termix_plan_lists_keys_and_how_to_select_them() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let args = ImportArgs {
        source: SourceArg::Termix,
        ..cli(fixture())
    };
    let text = run_with(&engine, dir.path(), args.clone(), Scripted::default())
        .await
        .unwrap();
    assert!(
        text.starts_with(
            "Termix import: termix_export.json (Termix JSON)\n10 records: 8 hosts to import, 2 skipped.\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "\nPrivate keys: 3 found, 0 selected (none is imported unless selected)\n\
             \x20 ssh-ed25519          {ED25519}\n\
             \x20   OpenSSH, not encrypted · used by hbd, hbd-backup\n\
             \x20 ssh-ed25519          {LOCKED}\n\
             \x20   OpenSSH, encrypted · fingerprint not verified (encrypted key) · used by locked-box\n\
             \x20 ssh-rsa              {RSA}\n\
             \x20   PEM, not encrypted · used by legacy-rsa\n\
             \x20 Select keys with --key SHA256:… (repeatable) or --all-keys."
        )),
        "{text}"
    );
    assert!(
        text.contains("— `tunnelConnections` (1) was not imported"),
        "{text}"
    );
    assert!(
        text.contains("Not imported: 3 passwords, 1 unreadable key, 1 other key."),
        "{text}"
    );
    assert!(
        text.ends_with(&format!(
            "Run again with --apply: portico import termix {} --apply\n",
            shell_word(&fixture().display().to_string())
        )),
        "{text}"
    );
    assert_no_secret(&text);
    assert!(!dir.path().join("config/hosts.conf").exists());
    termix_keys_selected(&engine, dir.path(), &args).await;
}

#[tokio::test]
async fn termix_plan_numbers_records_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let text = run_with(&engine, dir.path(), cli(fixture()), Scripted::default())
        .await
        .unwrap();
    // JSON records are numbered `#n` (not lines), in record order.
    assert!(
        text.contains("\nSkipped\n  #9 · inject ip — ")
            && text.contains("\n  #10 · inject user — "),
        "{text}"
    );
    assert!(!text.contains("  line "), "{text}");
    let warnings: Vec<usize> = text
        .split("\nWarnings\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|l| l.starts_with("  #"))
        .map(|l| l[3..].split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(warnings, [5, 6, 6, 6, 7], "{text}");
    // The alias column fits the longest alias (32 at most).
    assert!(
        text.contains("\n  hbd                              root@203.0.113.20:2222\n"),
        "{text}"
    );
    assert!(
        text.contains("\n  evilproxycommand-touch-tmp-pwned root@192.0.2.66\n"),
        "{text}"
    );
}

/// The Termix plan with keys selected (one, all, an unknown one).
async fn termix_keys_selected(engine: &Engine, home: &Path, args: &ImportArgs) {
    let dir = home;
    let args = args.clone();
    // Selected keys are marked, and the suggested command keeps them.
    let selected = ImportArgs {
        keys: vec![RSA.to_owned()],
        ..args.clone()
    };
    let text = run_with(engine, dir, selected, Scripted::default())
        .await
        .unwrap();
    assert!(text.contains("3 found, 1 selected"), "{text}");
    assert!(text.contains(&format!("{RSA}  [selected]\n")), "{text}");
    assert!(!text.contains(&format!("{ED25519}  [selected]")), "{text}");
    assert!(
        text.contains("add --key-passphrase to encrypt them"),
        "{text}"
    );
    assert!(text.ends_with(&format!(" --key {RSA} --apply\n")), "{text}");

    let all = ImportArgs {
        all_keys: true,
        key_passphrase: true,
        ..args.clone()
    };
    let text = run_with(engine, dir, all, Scripted::default())
        .await
        .unwrap();
    assert!(text.contains("3 found, 3 selected"), "{text}");
    assert!(!text.contains("add --key-passphrase"), "{text}");
    assert!(
        text.ends_with(" --all-keys --key-passphrase --apply\n"),
        "{text}"
    );

    // A fingerprint the export does not have: refused, plan shown.
    let unknown = ImportArgs {
        keys: vec!["SHA256:AAAA".to_owned()],
        apply: true,
        ..args
    };
    let failure = run_with(engine, dir, unknown, Scripted::default())
        .await
        .unwrap_err();
    assert_eq!(failure.summary.code, "import_unknown_key");
    assert_eq!(failure.summary.message, "no key SHA256:AAAA in this export");
    assert!(failure.stdout.contains("Private keys: 3 found"));
    assert!(!dir.join("config/hosts.conf").exists());
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The fixture, applied with one key selected and a passphrase confirmed
/// twice: the key is written `0600` under `~/.ssh/portico`, encrypted,
/// and the two hosts using it point to it; no other key is written.
#[tokio::test]
async fn termix_apply_with_one_key_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let prompt = Scripted::new([Ok("first try"), Ok("typo"), Ok("s3cret"), Ok("s3cret")]);
    let args = ImportArgs {
        keys: vec![ED25519.to_owned()],
        key_passphrase: true,
        apply: true,
        ..cli(fixture())
    };
    let text = run_with(&engine, dir.path(), args, prompt.clone())
        .await
        .unwrap();
    // The plan is printed first, then the passphrase is asked.
    let asked = prompt.asked();
    let (plan, prompts) = asked.split_first().unwrap();
    assert!(
        plan.starts_with("show: Termix import: termix_export.json (Termix JSON)\n"),
        "{plan}"
    );
    assert!(plan.contains("Private keys: 3 found, 1 selected"), "{plan}");
    assert!(!plan.contains("Wrote"), "{plan}");
    assert_eq!(
        prompts,
        [
            "Passphrase for the imported keys: ",
            "Same passphrase again: ",
            "note: The two passphrases differ: try again.",
            "Passphrase for the imported keys: ",
            "Same passphrase again: ",
        ]
    );
    // What is left to print: the report alone, the action on the export
    // right after its summary, and again (short) on the last line.
    let key = dir.path().join(".ssh/portico/hbd");
    assert!(
        text.starts_with(
            "Wrote 8 hosts to ~/config/hosts.conf.\n▲ Delete termix_export.json now: it holds 3 passwords and 5 private keys (in clear text or encrypted).\n"
        ),
        "{text}"
    );
    assert!(!text.contains("Termix import:"), "{text}");
    assert!(!text.contains("\nSecrets\n"), "{text}");
    assert!(
        text.contains(
            "\nKeys imported\n  ~/.ssh/portico/hbd  written · 0600 · encrypted · used by hbd, hbd-backup\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\n2 keys not imported (not selected, or none of its hosts was imported).\n"),
        "{text}"
    );
    assert!(!text.contains("Keys reused"), "{text}");
    assert!(
        text.contains(
            "\nOptional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config:\n\n    Include ~/config/hosts.conf\n"
        ),
        "{text}"
    );
    assert!(!text.contains("One step left"), "{text}");
    assert!(
        text.ends_with("\n\n▲ Delete termix_export.json now.\n"),
        "{text}"
    );
    assert_no_secret(&text);
    assert!(!text.contains("s3cret"));

    #[cfg(unix)]
    {
        assert_eq!(mode(&key), 0o600);
        assert_eq!(mode(&dir.path().join(".ssh/portico")), 0o700);
    }
    let written =
        russh::keys::ssh_key::PrivateKey::from_openssh(std::fs::read_to_string(&key).unwrap())
            .unwrap();
    assert!(written.is_encrypted(), "encrypted with the passphrase");
    let opened = written.decrypt("s3cret").unwrap();
    assert_eq!(
        opened
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string(),
        ED25519
    );
    let entries: Vec<_> = std::fs::read_dir(dir.path().join(".ssh/portico"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(entries, ["hbd", "hbd.pub"], "only the selected key");
    let conf = std::fs::read_to_string(dir.path().join("config/hosts.conf")).unwrap();
    assert_eq!(
        conf.matches("IdentityFile ~/.ssh/portico/hbd\n").count(),
        2,
        "{conf}"
    );
    assert_no_secret(&conf);
}

#[tokio::test]
async fn passphrase_prompt_only_when_needed_and_never_half_written() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    // The encrypted key is written as it is: nothing to ask.
    let prompt = Scripted::default();
    let args = ImportArgs {
        keys: vec![LOCKED.to_owned()],
        key_passphrase: true,
        apply: true,
        ..cli(fixture())
    };
    let text = run_with(&engine, dir.path(), args, prompt.clone())
        .await
        .unwrap();
    assert!(prompt.asked().is_empty(), "{:?}", prompt.asked());
    // Encrypted in the export: written as it is, still encrypted.
    assert!(
        text.contains(
            "\nKeys imported\n  ~/.ssh/portico/locked-box  written · 0600 · encrypted · used by locked-box\n"
        ),
        "{text}"
    );

    // Interrupted, or never confirmed: refused, nothing written.
    let other = tempfile::tempdir().unwrap();
    let engine = self::engine(other.path());
    let args = ImportArgs {
        keys: vec![ED25519.to_owned()],
        key_passphrase: true,
        apply: true,
        ..cli(fixture())
    };
    let interrupted = Scripted::new([Err(io::ErrorKind::Interrupted.into())]);
    let failure = run_with(&engine, other.path(), args.clone(), interrupted.clone())
        .await
        .unwrap_err();
    assert_eq!(
        failure.summary.message,
        "passphrase not given: nothing was written"
    );
    // The plan was already printed, before the question: not twice.
    assert!(
        interrupted
            .asked()
            .first()
            .is_some_and(|p| p.contains("Private keys: 3 found, 1 selected"))
    );
    assert!(failure.stdout.is_empty(), "{}", failure.stdout);
    let never = Scripted::new([Ok(""), Ok("a"), Ok("b"), Ok("a"), Ok("c")]);
    let failure = run_with(&engine, other.path(), args, never.clone())
        .await
        .unwrap_err();
    assert_eq!(
        failure.summary.message,
        "no passphrase confirmed: nothing was written"
    );
    assert!(
        never.asked().contains(
            &"note: The passphrase is empty: type one, or press Ctrl-C to stop.".to_owned()
        )
    );
    assert!(!other.path().join("config/hosts.conf").exists());
    assert!(!other.path().join(".ssh/portico").exists());
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
        secrets: portico_core::import::IgnoredSecrets::default(),
        identity_files: vec!["~/.termius/ssh_keys/db".into()],
        target: "/t/hosts.conf".into(),
        include_line: "Include /t/hosts.conf".into(),
        replaces: None,
        keys: Vec::new(),
        key_material: portico_core::import::KeyMaterial::default(),
    };
    let text = plan_text(
        &plan,
        Path::new("/home/ana"),
        &cli("x"),
        &KeySelection::None,
    );
    assert!(!text.contains("Warnings"), "{text}");
    assert!(
        text.contains("Identity files not imported\n  ~/.termius/ssh_keys/db\n"),
        "{text}"
    );
}

#[test]
fn report_lists_reused_keys_and_new_warnings_only() {
    let shown_warning = ImportNotice {
        code: NoticeCode::KeyNotUsed,
        record: Some(7),
        label: Some("Stale key".into()),
        message: "already shown".into(),
    };
    let report = ImportReport {
        format: portico_core::import::ImportFormat::TermixJson,
        target: "/home/ana/.config/portico/hosts.conf".into(),
        unchanged: false,
        imported: vec!["hbd".into(), "web".into()],
        updated: vec!["web".into()],
        skipped: Vec::new(),
        warnings: vec![
            shown_warning.clone(),
            ImportNotice {
                code: NoticeCode::KeySkipped,
                record: None,
                label: Some(RSA.into()),
                message: "this key was selected, but none of its hosts was imported".into(),
            },
        ],
        secrets: portico_core::import::IgnoredSecrets {
            passwords: 0,
            keys: 0,
            unticked_keys: 0,
            unreadable_keys: 0,
            keys_in_export: 1,
        },
        identity_files: Vec::new(),
        meta_updated: 2,
        include_line: "Include ~/.config/portico/hosts.conf".into(),
        include_active: true,
        keys: portico_core::import::KeyOutcome {
            imported: Vec::new(),
            reused: vec!["/home/ana/.ssh/id_ed25519".into()],
            skipped: 1,
        },
    };
    let shown = Shown {
        warnings: vec![shown_warning],
        file_name: "termix.json".into(),
        format: Some(portico_core::import::ImportFormat::TermixJson),
        ..Shown::default()
    };
    let text = report_text(&report, &shown, Path::new("/home/ana"));
    assert!(
        text.starts_with(
            "Wrote 2 hosts to ~/.config/portico/hosts.conf, 1 updated from an earlier import.\n▲ Delete termix.json now: it holds 1 private key (in clear text or encrypted).\nMetadata (group, tags, notes) updated for 2 hosts in hosts.toml.\n"
        ),
        "{text}"
    );
    assert!(text.contains("\nUpdated hosts\n  web\n"), "{text}");
    assert!(
        text.contains("\nKeys reused (already on this machine)\n  ~/.ssh/id_ed25519\n"),
        "{text}"
    );
    assert!(!text.contains("Keys imported"), "{text}");
    assert!(text.contains("\n1 key not imported"), "{text}");
    assert!(
        text.contains(&format!(
            "\nWarnings while writing\n  {RSA} — this key was selected, but none of its hosts was imported\n"
        )),
        "{text}"
    );
    assert!(!text.contains("already shown"), "{text}");
    assert!(
        text.ends_with(
            "already includes this file: the hosts are in your list.\n\n▲ Delete termix.json now.\n"
        ),
        "{text}"
    );
}

#[test]
fn alias_column_is_capped_and_notices_are_plain() {
    let mut plan = redirected_plan();
    if let Some(host) = plan.hosts.first_mut() {
        host.alias = "a".repeat(40);
    }
    let text = plan_text(
        &plan,
        Path::new("/home/ana"),
        &cli("x.json"),
        &KeySelection::None,
    );
    // Longer than 32: one space, the other rows padded to 32.
    assert!(
        text.contains(&format!("\n  {} deploy@203.0.113.20:2200", "a".repeat(40))),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "\n  web-prod{} deploy@198.51.100.10",
            " ".repeat(24)
        )),
        "{text}"
    );
    // `edited_since_import`: the core's message, nothing added.
    let notice = ImportNotice {
        code: NoticeCode::EditedSinceImport,
        record: Some(4),
        label: Some("Web".into()),
        message: "`web` was imported earlier, then edited: left as it is".into(),
    };
    assert_eq!(
        notice_line(&notice, portico_core::import::ImportFormat::TermixJson),
        "  #4 · Web — `web` was imported earlier, then edited: left as it is"
    );
    assert_eq!(
        notice_line(&notice, portico_core::import::ImportFormat::TemplateCsv),
        "  line 4 · Web — `web` was imported earlier, then edited: left as it is"
    );
}

/// An accepted destination change whose earlier key (written by an
/// import) no host uses any more says so; a key still used does not.
#[test]
fn report_says_when_a_redirect_leaves_a_key_unused() {
    let home = Path::new("/home/ana");
    let mut plan = redirected_plan();
    let old_key = PathBuf::from("/home/ana/.ssh/portico/web-prod");
    if let Some(host) = plan.hosts.get_mut(1)
        && let Some(previous) = &mut host.previous
    {
        previous.identity_file = Some(old_key.clone());
    }
    let accepted = ImportArgs {
        accept_redirect: vec!["web-prod".into()],
        ..cli("export.json")
    };
    let mut shown = Shown::of_plan(&plan, home, &accepted);
    assert_eq!(
        shown.previous_keys,
        [("web-prod".to_owned(), old_key.clone())]
    );
    let report = ImportReport {
        format: portico_core::import::ImportFormat::TermixJson,
        target: "/home/ana/.config/portico/hosts.conf".into(),
        unchanged: false,
        imported: vec!["hbd".into(), "web-prod".into()],
        updated: vec!["hbd".into(), "web-prod".into()],
        skipped: Vec::new(),
        warnings: Vec::new(),
        secrets: portico_core::import::IgnoredSecrets::default(),
        identity_files: Vec::new(),
        meta_updated: 0,
        include_line: "Include ~/.config/portico/hosts.conf".into(),
        include_active: true,
        keys: portico_core::import::KeyOutcome::default(),
    };
    let text = report_text(&report, &shown, home);
    assert!(
        text.contains(
            "\nUpdated hosts\n  hbd\n  web-prod — key ~/.ssh/portico/web-prod no longer used\n"
        ),
        "{text}"
    );
    assert!(!text.contains("Metadata"), "{text}");
    assert!(!text.contains("Delete"), "nothing to delete: {text}");
    // Another host still uses it: nothing to say.
    shown.key_users.insert(old_key, vec!["web-staging".into()]);
    let text = report_text(&report, &shown, home);
    assert!(text.contains("\n  web-prod\n"), "{text}");
    // Skipped (not accepted): never said.
    let skipped = Shown::of_plan(&plan, home, &cli("export.json"));
    assert!(skipped.previous_keys.is_empty());
}

/// A host of `plan` as a destination change of `web-prod`.
fn redirected_plan() -> ImportPlan {
    let host = PlannedHost {
        alias: "web-prod".into(),
        label: "Web Prod".into(),
        hostname: "198.51.100.99".into(),
        user: Some("deploy".into()),
        port: Some(2200),
        group: None,
        tags: vec!["web".into()],
        notes: None,
        record: 6,
        favorite: false,
        key: None,
        update: true,
        previous: Some(portico_core::managed_hosts::ManagedHost {
            alias: "web-prod".into(),
            hostname: "198.51.100.10".into(),
            user: Some("deploy".into()),
            port: None,
            identity_file: None,
            proxy_jump: None,
        }),
        source_id: "6".into(),
    };
    let same = PlannedHost {
        alias: "hbd".into(),
        label: "Hbd".into(),
        hostname: "203.0.113.20".into(),
        previous: None,
        tags: Vec::new(),
        record: 1,
        source_id: "1".into(),
        ..host.clone()
    };
    ImportPlan {
        source: "export.json".into(),
        format: portico_core::import::ImportFormat::TermixJson,
        options: ImportOptions::default(),
        records: 2,
        hosts: vec![same, host],
        skipped: Vec::new(),
        warnings: vec![ImportNotice {
            code: NoticeCode::DestinationChanged,
            record: Some(6),
            label: Some("Web Prod".into()),
            message: "`web-prod` was imported earlier for deploy@198.51.100.10: the export now points it elsewhere".into(),
        }],
        secrets: portico_core::import::IgnoredSecrets::default(),
        identity_files: Vec::new(),
        target: "/home/ana/.config/portico/hosts.conf".into(),
        include_line: "Include ~/.config/portico/hosts.conf".into(),
        replaces: Some(2),
        keys: Vec::new(),
        key_material: portico_core::import::KeyMaterial::default(),
    }
}

#[test]
fn plan_shows_destination_changes() {
    let plan = redirected_plan();
    let home = Path::new("/home/ana");
    let text = plan_text(&plan, home, &cli("export.json"), &KeySelection::None);
    assert!(
        text.contains("\n2 records: 1 host to import, 1 skipped.\n"),
        "{text}"
    );
    // Only the hosts actually rewritten count: not the skipped change.
    assert!(
        text.contains("\nUpdates 1 host from an earlier import.\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\n  web-prod deploy@198.51.100.10 → deploy@198.51.100.99:2200 #web  (destination changed, skipped)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\n  hbd      deploy@203.0.113.20:2200  (update)\n"),
        "{text}"
    );
    assert!(
        text.contains("unless you add --accept-redirect ALIAS"),
        "{text}"
    );
    // The host line says it: no repeated warning.
    assert!(!text.contains("Warnings"), "{text}");
    assert!(
        text.ends_with("Run again with --apply: portico import export.json --apply\n"),
        "{text}"
    );

    // Accepted: marked so, counted, and kept in the suggested command.
    let accepted = ImportArgs {
        accept_redirect: vec!["web-prod".into()],
        ..cli("export.json")
    };
    let text = plan_text(&plan, home, &accepted, &KeySelection::None);
    assert!(text.contains("2 hosts to import, 0 skipped"), "{text}");
    assert!(
        text.contains("\nUpdates 2 hosts from an earlier import.\n"),
        "{text}"
    );
    assert!(
        text.contains(":2200 #web  (destination changed, accepted)\n"),
        "{text}"
    );
    assert!(!text.contains("unless you add"), "{text}");
    assert!(
        text.ends_with(" --accept-redirect web-prod --apply\n"),
        "{text}"
    );
}

#[test]
fn report_lists_skipped_destination_changes() {
    let plan = redirected_plan();
    let report = ImportReport {
        format: portico_core::import::ImportFormat::TermixJson,
        target: "/home/ana/.config/portico/hosts.conf".into(),
        unchanged: false,
        imported: vec!["hbd".into()],
        updated: vec!["hbd".into()],
        skipped: vec![ImportNotice {
            code: NoticeCode::DestinationChanged,
            record: Some(6),
            label: Some("Web Prod".into()),
            message: "this alias was imported earlier for deploy@198.51.100.10: not pointed elsewhere without your consent".into(),
        }],
        warnings: Vec::new(),
        secrets: portico_core::import::IgnoredSecrets::default(),
        identity_files: Vec::new(),
        meta_updated: 1,
        include_line: "Include ~/.config/portico/hosts.conf".into(),
        include_active: true,
        keys: portico_core::import::KeyOutcome::default(),
    };
    let shown = Shown {
        redirects: plan
            .hosts
            .iter()
            .filter(|h| h.previous.is_some())
            .map(|h| (h.record, h.alias.clone()))
            .collect(),
        ..Shown::default()
    };
    let text = report_text(&report, &shown, Path::new("/home/ana"));
    assert!(
        text.contains(
            "\nDestination changes not applied (still pointing at the earlier server)\n  web-prod — this alias was imported earlier for deploy@198.51.100.10: not pointed elsewhere without your consent\n  To apply one, run the import again with --accept-redirect ALIAS.\n"
        ),
        "{text}"
    );
    assert!(!text.contains("changed since the plan"), "{text}");
    assert!(!text.contains("Delete the export"), "{text}");
}

/// The fixture with `Web Prod` moved to another server.
fn moved_fixture(dir: &Path) -> PathBuf {
    let text = std::fs::read_to_string(fixture()).unwrap();
    assert!(text.contains("\"198.51.100.10\""));
    let moved = dir.join("termix_export.json");
    std::fs::write(
        &moved,
        text.replace("\"198.51.100.10\"", "\"198.51.100.99\""),
    )
    .unwrap();
    moved
}

fn hosts_conf(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("config/hosts.conf")).unwrap()
}

/// Import the fixture, then a modified export that points `web-prod`
/// elsewhere: skipped without `--accept-redirect`, applied with it.
#[tokio::test]
async fn reimport_with_a_destination_change_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let first = ImportArgs {
        apply: true,
        ..cli(fixture())
    };
    run_with(&engine, dir.path(), first, Scripted::default())
        .await
        .unwrap();
    assert!(hosts_conf(dir.path()).contains("HostName 198.51.100.10\n"));

    let moved = moved_fixture(dir.path());
    // The plan shows the change, marked skipped.
    let text = run_with(&engine, dir.path(), cli(&moved), Scripted::default())
        .await
        .unwrap();
    assert!(
        text.contains("deploy@198.51.100.10 → deploy@198.51.100.99"),
        "{text}"
    );
    assert!(text.contains("(destination changed, skipped)\n"), "{text}");

    // An alias that is not a destination change: refused, nothing written.
    let before = hosts_conf(dir.path());
    let failure = run_with(
        &engine,
        dir.path(),
        ImportArgs {
            apply: true,
            accept_redirect: vec!["hbd".into()],
            ..cli(&moved)
        },
        Scripted::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.summary.code, "import_unknown_redirect");
    assert_eq!(
        failure.summary.message,
        "`hbd` is not a destination change of this import"
    );
    assert!(failure.stdout.contains("(destination changed, skipped)"));
    assert_eq!(hosts_conf(dir.path()), before);

    // Applied without consent: the host keeps its old destination.
    let text = run_with(
        &engine,
        dir.path(),
        ImportArgs {
            apply: true,
            ..cli(&moved)
        },
        Scripted::default(),
    )
    .await
    .unwrap();
    assert!(
        text.contains(
            "\nDestination changes not applied (still pointing at the earlier server)\n  web-prod — "
        ),
        "{text}"
    );
    assert!(text.contains("--accept-redirect ALIAS"), "{text}");
    let conf = hosts_conf(dir.path());
    assert!(conf.contains("HostName 198.51.100.10\n"), "{conf}");
    assert!(!conf.contains("198.51.100.99"), "{conf}");

    // With consent (any case): pointed at the new server.
    let text = run_with(
        &engine,
        dir.path(),
        ImportArgs {
            apply: true,
            accept_redirect: vec!["web-prod".into()],
            ..cli(&moved)
        },
        Scripted::default(),
    )
    .await
    .unwrap();
    assert!(text.contains("(destination changed, accepted)\n"), "{text}");
    assert!(!text.contains("Destination changes not applied"), "{text}");
    let conf = hosts_conf(dir.path());
    assert!(conf.contains("HostName 198.51.100.99\n"), "{conf}");
    assert!(!conf.contains("198.51.100.10"), "{conf}");
    assert_no_secret(&text);
}
