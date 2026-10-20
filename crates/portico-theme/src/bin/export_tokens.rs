//! Regenerates `apps/portico-gui/src/lib/styles/tokens.css` from the Rust tokens.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/portico-gui/src/lib/styles/tokens.css");
    std::fs::write(&path, portico_theme::export_css())?;
    #[allow(clippy::print_stdout)]
    {
        println!("wrote {}", path.display());
    }
    Ok(())
}
