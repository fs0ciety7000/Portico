//! `portico`: the Portico terminal UI.
//!
//! Architecture (see `.claude/skills/tui-patterns/SKILL.md`):
//! `Event (key / engine / tick) → Action → App::update → App::view`.
//! Rendering is a pure function of [`app::App`]; all I/O goes through the
//! `portico_core::Engine`.

mod action;
mod app;
mod app_panels;
mod copy_job;
mod dialog;
mod files;
mod form;
mod keymap;
mod keys;
mod snippets;
mod terminal;
mod theme;
mod tunnels;
mod ui;

use color_eyre::Result;
use portico_core::{Engine, EngineConfig};

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();

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
