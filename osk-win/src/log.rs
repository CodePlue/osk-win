//! The single print site for osk-win's UI and dictation paths:
//! user-facing warnings and errors.
//!
//! osk-win is a GUI-subsystem program, so stderr usually goes nowhere;
//! every warning is also appended to `%LOCALAPPDATA%\osk\osk.log`.

use std::io::Write;
use std::path::PathBuf;

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA"))?;
    Some(PathBuf::from(base).join("osk").join("osk.log"))
}

/// Print a user-facing warning or error to stderr and the log file.
/// Modules outside main.rs print only through this; call sites use the
/// `warn!` macro defined at the crate root.
#[allow(clippy::print_stderr)] // the one authorized print site outside main.rs
pub(crate) fn warn(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
    let Some(path) = log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{args}");
    }
}
