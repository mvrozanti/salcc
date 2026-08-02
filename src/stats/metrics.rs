//! Append-only JSONL cost log at $XDG_STATE_HOME/salcc/metrics.jsonl.
//! One `Record` per scoring decision (API call or cache hit). Writes are
//! sync under a mutex — call rate is per-block, tiny. `read_all` is the
//! reader API for the M4 stats server; it skips unparseable lines so a
//! torn final write never poisons the dashboard. Append failures are
//! logged (tracing, file sink) and swallowed: cost observability must
//! never take down the proxy.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub ts: u64,
    pub session: String,
    pub block_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
    pub spans_returned: usize,
    pub cache_hit: bool,
}

impl Record {
    pub fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct MetricsLog {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl MetricsLog {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn open_default() -> Self {
        let path = default_path().unwrap_or_else(|| PathBuf::from("salcc-metrics.jsonl"));
        Self::at(path)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn append(&self, record: &Record) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = self.try_append(record) {
            tracing::warn!(%err, path = ?self.path, "metrics append failed; record dropped");
        }
    }

    fn try_append(&self, record: &Record) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    pub fn read_all(&self) -> Vec<Record> {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        bytes
            .split(|&b| b == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect()
    }
}

fn default_path() -> Option<PathBuf> {
    Some(dirs::state_dir()?.join("salcc/metrics.jsonl"))
}
