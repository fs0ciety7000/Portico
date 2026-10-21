//! `portico`: the Portico terminal UI.
//!
//! Architecture (see `.claude/skills/tui-patterns/SKILL.md`):
//! `Event (key / engine / tick) → Action → App::update → App::view`.
//! Rendering is a pure function of [`app::App`]; all I/O goes through the
//! `portico_core::Engine`.
//!
//! Command line: `--help`, `--version`, `--print-keybindings` (the TUI
//! tables of `docs/KEYBINDINGS.md`) and `--write-keybindings <file>`
//! (regenerates that block in place). Without a terminal on stdin and
//! stdout, Portico says so and exits with status 1.

mod action;
mod app;
mod app_meta;
mod app_panels;
mod copy_job;
mod debug;
mod dialog;
mod edit_host;
mod files;
mod form;
mod host_list;
mod keydoc;
mod keymap;
mod keys;
mod snippets;
mod terminal;
mod theme;
mod tunnels;
mod ui;

use std::io::IsTerminal;
use std::process::ExitCode;

use color_eyre::Result;
use portico_core::{Engine, EngineConfig};

const USAGE: &str = "\
Portico: SSH sessions, keyboard-first.

Usage: portico [OPTION]

Options:
  -h, --help                   Show this help
  -V, --version                Show the version
      --print-keybindings      Print the TUI keyboard shortcuts (Markdown)
      --write-keybindings FILE Regenerate the TUI block of FILE
                               (docs/KEYBINDINGS.md)

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
    /// Unknown option or missing value: message for stderr.
    Usage(String),
}

fn parse_args(args: &[String]) -> Command {
    match args {
        [] => Command::Run,
        [one] => match one.as_str() {
            "-h" | "--help" => Command::Help,
            "-V" | "--version" => Command::Version,
            "--print-keybindings" => Command::PrintKeybindings,
            "--write-keybindings" => Command::Usage("--write-keybindings needs a file".into()),
            other => Command::Usage(format!("unknown option: {other}")),
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
        Command::Usage(message) => {
            eprintln!("portico: {message}\n\n{USAGE}");
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
/// controls the filter, e.g. `PORTICO_LOG=portico_core=debug`.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let Ok(filter) = EnvFilter::try_from_env("PORTICO_LOG") else {
        return;
    };
    let path = std::env::temp_dir().join("portico-tui.log");
    if let Ok(file) = std::fs::File::create(path) {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_args(&args(&["--write-keybindings"])),
            Command::Usage(_)
        ));
    }
}
