//! Predictor history persistence, the Windows counterpart of the Linux
//! osk-backend's: the same file format and atomic write, kept where
//! osk-win has always kept it.

use std::path::{Path, PathBuf};

use oskwin_core::predict::Predictor;

/// `%APPDATA%\osk\user_history.tsv`, else `%LOCALAPPDATA%`, else the
/// profile's roaming AppData.
pub fn history_path() -> PathBuf {
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Some(dir) = std::env::var_os(var) {
            let p = PathBuf::from(dir);
            if p.is_absolute() {
                return p.join("osk").join("user_history.tsv");
            }
        }
    }
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    profile.join("AppData").join("Roaming").join("osk").join("user_history.tsv")
}

/// Merge the learned history stored at [`history_path`] into `p`. A
/// missing file (first run) is the normal silent case.
pub fn load_history(p: &mut Predictor) {
    let Ok(text) = std::fs::read_to_string(history_path()) else {
        return;
    };
    p.load_history_tsv(&text);
}

/// Write the learned history when `p` has flagged it dirty. A failed
/// write leaves it flagged so the next save retries, and the error is
/// returned for the host to report.
pub fn save_history_if_dirty(p: &mut Predictor) -> std::io::Result<()> {
    if !p.take_history_dirty() {
        return Ok(());
    }
    let res = write_history_tsv(&p.history_tsv(), &history_path());
    if res.is_err() {
        p.mark_history_dirty();
    }
    res
}

/// Parent dirs created, content written to a `.tmp` sibling and
/// renamed; when that fails, a plain write is the fallback.
fn write_history_tsv(text: &str, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)).is_ok() {
        return Ok(());
    }
    let res = std::fs::write(path, text);
    let _ = std::fs::remove_file(&tmp);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("oskwin-history-{}", std::process::id()));
        let path = dir.join("osk").join("user_history.tsv");
        let mut p = Predictor::ephemeral();
        p.commit("hello");
        p.commit("world");
        write_history_tsv(&p.history_tsv(), &path).unwrap();
        let mut q = Predictor::ephemeral();
        q.load_history_tsv(&std::fs::read_to_string(&path).unwrap());
        assert!(q.is_learned("hello") && q.is_learned("world"));
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
