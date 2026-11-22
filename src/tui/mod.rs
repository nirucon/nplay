//! ratatui terminal UI (port of ui.py, ui_render.py, ui_common.py). Owner: tui track.

use crate::app::App;
use std::sync::Arc;

/// Run the terminal UI until quit; `query` (may be empty) starts a search.
/// Returns the process exit code. The caller runs `App::shutdown` after it.
pub fn run(_app: Arc<App>, _query: &str) -> i32 {
    eprintln!("NPLAY terminal UI: not implemented yet");
    1
}
