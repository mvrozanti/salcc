//! Polling tail over the active session file. Attach seats the byte offset
//! at EOF so history is never scored — only blocks appended after attach.
//! Each poll reads from the stored offset and consumes complete lines only:
//! a torn final line leaves the offset at that line's start, retried next
//! poll (75ms). Rotation: when discovery (project_dir set) finds a strictly
//! newer session file, the reader switches to it at offset 0 — a fresh
//! session's content is all new. Truncation of the current file is treated
//! the same way (offset 0). `poll` is sync and unit-testable; `spawn` wraps
//! it in a tokio task yielding TextBlocks over an unbounded mpsc channel and
//! exits when the receiver drops.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;

use super::discover::newest_jsonl;
use super::model::{self, TextBlock};

const POLL_INTERVAL: Duration = Duration::from_millis(75);

#[derive(Debug)]
pub struct TailReader {
    session_path: PathBuf,
    project_dir: Option<PathBuf>,
    offset: u64,
}

impl TailReader {
    pub fn attach(session_path: PathBuf, project_dir: Option<PathBuf>) -> std::io::Result<Self> {
        let offset = std::fs::metadata(&session_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        tracing::debug!(?session_path, offset, "tail attached at EOF");
        Ok(Self {
            session_path,
            project_dir,
            offset,
        })
    }

    pub fn session_path(&self) -> &PathBuf {
        &self.session_path
    }

    pub fn poll(&mut self) -> Vec<TextBlock> {
        self.check_rotation();
        let mut blocks = Vec::new();
        let Ok(mut file) = std::fs::File::open(&self.session_path) else {
            return blocks;
        };
        let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        if len < self.offset {
            tracing::debug!(path = ?self.session_path, "session file shrank; rewinding to start");
            self.offset = 0;
        }
        if len == self.offset {
            return blocks;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return blocks;
        }
        let mut tail = Vec::with_capacity((len - self.offset) as usize);
        if file.read_to_end(&mut tail).is_err() {
            return blocks;
        }
        let Some(last_newline) = tail.iter().rposition(|&b| b == b'\n') else {
            return blocks;
        };
        let complete = &tail[..=last_newline];
        for line in complete.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if let Some(block) = model::text_blocks(text) {
                blocks.push(block);
            }
        }
        self.offset += (last_newline + 1) as u64;
        blocks
    }

    fn check_rotation(&mut self) {
        let Some(dir) = &self.project_dir else {
            return;
        };
        let Some((newest, newest_mtime)) = newest_jsonl(dir) else {
            return;
        };
        if newest == self.session_path {
            return;
        }
        let current_mtime = std::fs::metadata(&self.session_path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest_mtime > current_mtime {
            tracing::debug!(from = ?self.session_path, to = ?newest, "session rotated");
            self.session_path = newest;
            self.offset = 0;
        }
    }

    pub fn spawn(mut self) -> mpsc::UnboundedReceiver<TextBlock> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                for block in self.poll() {
                    if tx.send(block).is_err() {
                        tracing::debug!("tail receiver dropped; reader task exiting");
                        return;
                    }
                }
                if tx.is_closed() {
                    tracing::debug!("tail receiver dropped; reader task exiting");
                    return;
                }
            }
        });
        rx
    }
}
