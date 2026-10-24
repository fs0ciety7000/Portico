//! `portico`: the Portico terminal UI.
//!
//! Architecture (see `.claude/skills/tui-patterns/SKILL.md`):
//! `Event (key / engine / tick) → Action → App::update → App::view`.
//! Rendering is a pure function of [`app::App`]; all I/O goes through the
//! `portico_core::Engine`.
//!
//! Command line: `--help`, `--version`, `--print-keybindings` (the TUI
//! tables of `docs/KEYBINDINGS.md`) and `--write-keybindings <file>`
//! (regenerates that block in place), and `import [termius|termix] FILE
//! [--prefix] [--key SHA256:…] [--all-keys] [--key-passphrase]
//! [--accept-redirect ALIAS] [--apply]`
//! (see [`import_cli`]). Without a terminal on stdin and
//! stdout, Portico says so and exits with status 1.

mod action;
mod app;
mod app_hosts;
mod app_meta;
mod app_panels;
mod app_panes;
mod app_recording;
mod app_update;
mod appearance;
mod copy_job;
mod debug;
mod dialog;
mod edit_host;
mod files;
mod form;
mod host_form;
mod host_list;
mod import_cli;
mod keydoc;
mod keymap;
mod keys;
mod panes;
mod player;
mod recordings;
mod snippets;
mod term_search;
mod terminal;
mod theme;
mod tunnels;
mod ui;

/// The in-process SSH server and engine harness of `portico-core`'s
/// integration tests, shared so TUI tests drive real sessions (no network).
#[cfg(test)]
#[path = "../../../crates/portico-core/tests/support/mod.rs"]
mod ssh_support;

use std::io::IsTerminal;
use std::process::ExitCode;

use color_eyre::Result;
use portico_core::{Engine, EngineConfig};

const USAGE: &str = "\
Portico: SSH sessions, keyboard-first.

Usage: portico [OPTION]
       portico import [termius|termix] FILE [--prefix] [--key SHA256:...]...
                      [--all-keys] [--key-passphrase]
                      [--accept-redirect ALIAS]... [--apply]

Options:
  -h, --help                   Show this help
  -V, --version                Show the version
      --print-keybindings      Print the TUI keyboard shortcuts (Markdown)
      --write-keybindings FILE Regenerate the TUI block of FILE
                               (docs/KEYBINDINGS.md)

Import from Termius or Termix (see docs/IMPORT.md):
  import FILE                  Show what importing FILE would do. The format
                               is read from the content: a Termius CSV,
                               termius-local-export hosts.json or
                               credentials.csv, `termius export-ssh-config`
                               output, or a Termix JSON export. Lists the
                               hosts and the private keys found. Nothing is
                               written. `import termius FILE` and
                               `import termix FILE` (Termix only) also work.
      --prefix                 Prefix every alias with `termius-` / `termix-`
      --key SHA256:...         Import this private key (fingerprint from the
                               plan; repeatable). No key is imported unless
                               selected: it goes to ~/.ssh/portico (0600),
                               or a local copy with the same fingerprint is
                               reused.
      --all-keys               Import every private key of the export
      --key-passphrase         Ask on the terminal (twice, not echoed) for a
                               passphrase that encrypts the imported keys.
                               Never given on the command line.
      --accept-redirect ALIAS  Apply this destination change: a re-import
                               that points ALIAS (imported earlier) at
                               another HostName, User or Port. Skipped
                               otherwise (repeatable)
      --apply                  Write the hosts to Portico's hosts.conf and
                               print the Include line to add to
                               ~/.ssh/config yourself (never edited by
                               Portico)

Without an option, Portico opens in the current terminal. Settings live in
settings.toml (theme, [keymap.tui]); see docs/KEYBINDINGS.md.
";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run,
    Help,
    Version,
    PrintKeybindings,
    WriteKeybindings(std::path::PathBuf),
    /// `import [termius|termix] FILE [--prefix] [--key ID]… [--all-keys]
    /// [--key-passphrase] [--accept-redirect ALIAS]… [--apply]`.
    Import(import_cli::ImportArgs),
    /// Unknown option or missing value: message for stderr.
    Usage(String),
}

fn parse_args(args: &[String]) -> Command {
    match args {
        [] => Command::Run,
        [one] if one != "import" => match one.as_str() {
            "-h" | "--help" => Command::Help,
            "-V" | "--version" => Command::Version,
            "--print-keybindings" => Command::PrintKeybindings,
            "--write-keybindings" => Command::Usage("--write-keybindings needs a file".into()),
            other => Command::Usage(format!("unknown option: {other}")),
        },
        // `portico import --help` (anywhere after `import`): the usage.
        [command, rest @ ..]
            if command == "import" && rest.iter().any(|a| a == "-h" || a == "--help") =>
        {
            Command::Help
        }
        [command, rest @ ..] if command == "import" => match import_cli::parse(rest) {
            Ok(args) => Command::Import(args),
            Err(message) => Command::Usage(message),
        },
        [flag, path] if flag == "--write-keybindings" => {
            Command::WriteKeybindings(std::path::PathBuf::from(path))
        }
        _ => Command::Usage("too many arguments".into()),
    }
}

// The CLI answers on stdout / stderr by design (no TUI is drawn there).
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() -> Result<ExitCode> {
    // Before any thread exists (see `init_local_offset`).
    ui::fmt::init_local_offset();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Command::Help => {
            print!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Command::Version => {
            println!("portico {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Command::PrintKeybindings => {
            print!("{}", keydoc::block());
            return Ok(ExitCode::SUCCESS);
        }
        Command::WriteKeybindings(path) => {
            return Ok(match keydoc::write(&path) {
                Ok(()) => {
                    println!("updated {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("portico: {e}");
                    ExitCode::FAILURE
                }
            });
        }
        Command::Import(args) => {
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(import_cli::run(args));
            return Ok(match outcome {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(import_cli::Failure { stdout, summary }) => {
                    print!("{stdout}");
                    eprintln!("portico: {}", summary.message);
                    if let Some(hint) = summary.hint {
                        eprintln!("  {hint}");
                    }
                    ExitCode::FAILURE
                }
            });
        }
        Command::Usage(message) => {
            eprintln!("portico: {message}. Try `portico --help`.");
            return Ok(ExitCode::from(2));
        }
        Command::Run => {}
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        eprintln!(
            "portico needs a terminal: run it in an interactive terminal (stdin and stdout must be a TTY). See portico --help."
        );
        return Ok(ExitCode::FAILURE);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_tui())?;
    Ok(ExitCode::SUCCESS)
}

async fn run_tui() -> Result<()> {
    // First, before any secret can be in memory: no core dumps, no ptrace.
    let hardening = portico_core::hardening::harden_process();
    color_eyre::install()?;
    init_tracing();
    hardening.log();

    let engine = Engine::new(EngineConfig::discover()?);
    let terminal = ratatui::init();
    // Pastes arrive as one event (and can be bracketed for the remote shell).
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
    let result = app::App::new(engine).run(terminal).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    result
}

/// Logs go to a file (never to the terminal we draw on). `PORTICO_LOG`
/// controls the filter, e.g. `PORTICO_LOG=portico_core=debug`. The file
/// lives in Portico's data dir (`portico-tui.log`, private), not in a
/// shared `/tmp` where another account could read or pre-create it.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let Ok(filter) = EnvFilter::try_from_env("PORTICO_LOG") else {
        return;
    };
    let Ok(paths) = portico_core::store::Paths::discover() else {
        return;
    };
    let opened = open_private_log(&paths.data_dir);
    if let Err(e) = &opened {
        // Before the TUI takes the terminal: stderr is still the user's.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("portico: no log file in {}: {e}", paths.data_dir.display());
        }
    }
    if let Ok(file) = opened {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    }
}

/// Opens `<dir>/portico-tui.log` for this run: directory created `0700`,
/// file `0600`, and a symlink in its place is refused (never followed).
/// On Windows "private" is an owner-only DACL (portico-core's `0600`).
fn open_private_log(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    portico_core::store::create_private_dir_all(dir).map_err(std::io::Error::other)?;
    #[cfg(not(windows))]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let path = dir.join("portico-tui.log");
    if path
        .symlink_metadata()
        .is_ok_and(|m| !m.file_type().is_file())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "portico-tui.log is not a regular file",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    #[cfg(windows)]
    portico_core::store::windows::with_dacl_rights(&mut options);
    let file = options.open(&path)?;
    #[cfg(windows)]
    portico_core::store::windows::restrict_file(&file, &path)?;
    // An older file may have been created with looser permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn tui_log_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data");
        open_private_log(&dir).unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("portico-tui.log")), 0o600);
        // A symlink planted in its place is refused, its target untouched.
        let target = root.path().join("elsewhere");
        std::fs::write(&target, "keep").unwrap();
        std::fs::remove_file(dir.join("portico-tui.log")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("portico-tui.log")).unwrap();
        assert!(open_private_log(&dir).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn command_line() {
        assert_eq!(parse_args(&[]), Command::Run);
        assert_eq!(parse_args(&args(&["--version"])), Command::Version);
        assert_eq!(parse_args(&args(&["-h"])), Command::Help);
        assert_eq!(
            parse_args(&args(&["--print-keybindings"])),
            Command::PrintKeybindings
        );
        assert_eq!(
            parse_args(&args(&["--write-keybindings", "doc.md"])),
            Command::WriteKeybindings("doc.md".into())
        );
        assert!(matches!(parse_args(&args(&["--nope"])), Command::Usage(_)));
        assert!(matches!(
            parse_args(&args(&["import", "termius", "x.csv", "--apply"])),
            Command::Import(import_cli::ImportArgs { apply: true, .. })
        ));
        assert!(matches!(parse_args(&args(&["import"])), Command::Usage(_)));
        // `import --help` shows the usage (never "unknown option").
        for help in [
            &["import", "--help"][..],
            &["import", "-h"],
            &["import", "termix", "x.json", "--help"],
        ] {
            assert_eq!(parse_args(&args(help)), Command::Help, "{help:?}");
        }
        assert!(matches!(
            parse_args(&args(&["import", "termius"])),
            Command::Usage(_)
        ));
        assert!(matches!(
            parse_args(&args(&["import", "export.json", "--key-passphrase=x"])),
            Command::Usage(m) if !m.contains("=x")
        ));
        assert!(
            USAGE.contains("portico import [termius|termix] FILE [--prefix] [--key SHA256:...]...")
        );
        assert!(USAGE.contains("--key-passphrase"));
        assert!(USAGE.contains("--accept-redirect ALIAS"));
        assert!(matches!(
            parse_args(&args(&["import", "x.json", "--accept-redirect", "Web-Prod"])),
            Command::Import(import_cli::ImportArgs { accept_redirect, .. })
                if accept_redirect == ["web-prod"]
        ));
        assert!(matches!(
            parse_args(&args(&["--write-keybindings"])),
            Command::Usage(_)
        ));
    }
}
