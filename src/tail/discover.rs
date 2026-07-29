//! Session-file discovery. Claude Code writes transcripts to
//! ~/.claude/projects/<cwd with '/' replaced by '-'>/<session-uuid>.jsonl.
//! `discover_session` picks the most recently modified *.jsonl for a cwd;
//! `watch_for_new_session` polls (75ms) until a session file modified after
//! a given instant appears — needed right after child spawn, when the file
//! does not exist yet. The `*_in` variants take an explicit projects root
//! so tests never touch the real home directory. Watch takes SystemTime,
//! not Instant: file mtimes are SystemTime and the two clocks do not
//! convert; callers capture SystemTime::now() at spawn.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const POLL_INTERVAL: Duration = Duration::from_millis(75);

pub fn projects_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude/projects"))
}

pub fn project_dir(cwd: &Path) -> PathBuf {
    let root = projects_root().unwrap_or_else(|| PathBuf::from(".claude/projects"));
    project_dir_in(&root, cwd)
}

pub fn project_dir_in(root: &Path, cwd: &Path) -> PathBuf {
    root.join(munge(cwd))
}

fn munge(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

pub fn discover_session(cwd: &Path) -> Option<PathBuf> {
    let root = projects_root()?;
    discover_session_in(&root, cwd)
}

pub fn discover_session_in(root: &Path, cwd: &Path) -> Option<PathBuf> {
    newest_jsonl(&project_dir_in(root, cwd)).map(|(path, _)| path)
}

pub fn newest_jsonl(dir: &Path) -> Option<(PathBuf, SystemTime)> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .max_by_key(|(_, modified)| *modified)
}

pub async fn watch_for_new_session(cwd: &Path, after: SystemTime) -> PathBuf {
    let root = projects_root().unwrap_or_else(|| PathBuf::from(".claude/projects"));
    watch_for_new_session_in(&root, cwd, after).await
}

pub async fn watch_for_new_session_in(root: &Path, cwd: &Path, after: SystemTime) -> PathBuf {
    let dir = project_dir_in(root, cwd);
    loop {
        if let Some((path, modified)) = newest_jsonl(&dir)
            && modified > after
        {
            tracing::debug!(?path, "new session file appeared");
            return path;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
