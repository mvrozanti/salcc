//! CLI surface. `salcc <cmd...>` wraps a child; subcommands are reserved
//! words that cannot collide with wrapping (record/replay/stats/doctor).
//! Transform selection: `--no-style`, `NO_COLOR`, a non-tty stdout, or an
//! unknown terminal select the identity transform (bit-perfect passthrough);
//! otherwise the styling pipeline runs, and when the configured scorer is
//! "anthropic" the async scoring side channel is spawned before the child
//! (SystemTime captured pre-spawn so session discovery can watch for the
//! new transcript) and attached to the transform. A missing API key logs
//! one warning and degrades to heuristics-only — never a crash, never a
//! block. `record` tees raw child output into an asciinema cast v2 file
//! while passing through untouched; `replay` runs a cast through the
//! styling transform offline (cast gaps drive burst ends, verdicts settle
//! at EOF) and writes the styled bytes to stdout — the demo and debugging
//! tool for the emission channel.

use std::io::Write;
use std::time::{Instant, SystemTime};

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::emit::pipeline::{SessionSource, StylingTransform, spawn_side_channel};
use crate::proxy::io_loop::{self, ChunkTransform, IdentityTransform};
use crate::proxy::pty;
use crate::score::anthropic::AnthropicScorer;
use crate::stats::metrics::MetricsLog;
use crate::term::caps::Caps;

#[derive(Parser, Debug)]
#[command(name = "salcc", about = "Semantic attention layer for terminal AI agents", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Disable all styling (bit-perfect passthrough)
    #[arg(long, global = true)]
    pub no_style: bool,

    /// Child command to wrap, e.g. `salcc claude`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub child: Vec<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Record a raw session to a fixture file
    Record {
        #[arg(short, long)]
        output: std::path::PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        child: Vec<String>,
    },
    /// Replay a fixture through the styling pipeline
    Replay {
        fixture: std::path::PathBuf,
    },
    /// Show scoring cost summary; --serve starts the local dashboard
    Stats {
        #[arg(long)]
        serve: bool,
        #[arg(long, default_value_t = 7877)]
        port: u16,
    },
    /// Diagnose terminal capabilities and configuration
    Doctor {
        #[arg(long)]
        verify: bool,
    },
}

pub fn run(args: Cli) -> anyhow::Result<()> {
    match args.command {
        Some(Command::Record { output, child }) => run_record(&output, &child),
        Some(Command::Replay { fixture }) => run_replay(&fixture, args.no_style),
        Some(Command::Stats { .. }) => anyhow::bail!("stats: not yet implemented"),
        Some(Command::Doctor { .. }) => anyhow::bail!("doctor: not yet implemented"),
        None => {
            if args.child.is_empty() {
                anyhow::bail!("usage: salcc <command...>  (e.g. `salcc claude`)");
            }
            let code = run_proxy(&args.child, args.no_style)?;
            std::process::exit(code)
        }
    }
}

fn run_proxy(child: &[String], no_style: bool) -> anyhow::Result<i32> {
    let caps = Caps::detect_for_proxy();
    if no_style || !caps.styling_enabled {
        return io_loop::run(child, &mut IdentityTransform);
    }
    io_loop::init_tracing();
    let config = Config::load();
    let size = pty::real_terminal_size();
    let mut transform = StylingTransform::new(&config, caps, size.rows, size.cols);
    if config.scorer.name == "anthropic" {
        attach_anthropic_side_channel(&config, &mut transform);
    }
    io_loop::run(child, &mut transform)
}

fn attach_anthropic_side_channel(config: &Config, transform: &mut StylingTransform) {
    let metrics = MetricsLog::open_default();
    let scorer = match AnthropicScorer::new(&config.scorer, config.dimensions.clone(), metrics) {
        Ok(scorer) => scorer,
        Err(err) => {
            tracing::warn!(%err, "anthropic scorer unavailable; styling continues heuristics-only");
            return;
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            tracing::warn!(%err, "cwd unavailable; scoring side channel disabled");
            return;
        }
    };
    let source = SessionSource::Discover {
        cwd,
        spawned_at: SystemTime::now(),
    };
    transform.attach_score_channel(spawn_side_channel(source, Box::new(scorer)));
}

fn run_record(output: &std::path::Path, child: &[String]) -> anyhow::Result<()> {
    if child.is_empty() {
        anyhow::bail!("usage: salcc record -o <file.cast> <command...>");
    }
    let size = pty::real_terminal_size();
    let (width, height) = if size.cols > 0 && size.rows > 0 {
        (size.cols, size.rows)
    } else {
        (80, 24)
    };
    let mut recorder = RecordTransform::new(output, width, height)?;
    let code = io_loop::run(child, &mut recorder)?;
    recorder.close()?;
    std::process::exit(code)
}

struct RecordTransform {
    file: std::io::BufWriter<std::fs::File>,
    start: Instant,
    utf8_tail: Vec<u8>,
}

impl RecordTransform {
    fn new(path: &std::path::Path, width: u16, height: u16) -> anyhow::Result<Self> {
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        writeln!(file, "{{\"version\": 2, \"width\": {width}, \"height\": {height}}}")?;
        Ok(Self {
            file,
            start: Instant::now(),
            utf8_tail: Vec::new(),
        })
    }

    fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn write_event(&mut self, kind: &str, data: &str) {
        let line = serde_json::to_string(&(self.elapsed(), kind, data)).expect("event serializes");
        if writeln!(self.file, "{line}").is_err() {
            tracing::warn!("cast event write failed; recording is incomplete");
        }
    }

    fn record_output(&mut self, chunk: &[u8], final_flush: bool) {
        let mut bytes = std::mem::take(&mut self.utf8_tail);
        bytes.extend_from_slice(chunk);
        let split = if final_flush {
            bytes.len()
        } else {
            complete_utf8_prefix(&bytes)
        };
        self.utf8_tail = bytes[split..].to_vec();
        if split > 0 {
            let data = String::from_utf8_lossy(&bytes[..split]).into_owned();
            self.write_event("o", &data);
        }
    }

    fn close(&mut self) -> anyhow::Result<()> {
        self.file.flush()?;
        Ok(())
    }
}

impl ChunkTransform for RecordTransform {
    fn transform(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(chunk);
        self.record_output(chunk, false);
    }

    fn resize(&mut self, rows: u16, cols: u16, _out: &mut Vec<u8>) {
        self.write_event("r", &format!("{cols}x{rows}"));
    }

    fn finish(&mut self, _out: &mut Vec<u8>) {
        self.record_output(&[], true);
        if self.file.flush().is_err() {
            tracing::warn!("cast flush failed at stream end");
        }
    }
}

fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(err) => {
            if err.error_len().is_none() {
                err.valid_up_to()
            } else {
                bytes.len()
            }
        }
    }
}

const REPLAY_QUIET_GAP_SECS: f64 = 0.1;

fn run_replay(fixture: &std::path::Path, no_style: bool) -> anyhow::Result<()> {
    match replay_cast(fixture, no_style) {
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        other => other,
    }
}

fn replay_cast(fixture: &std::path::Path, no_style: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(fixture)?;
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(
        lines.next().ok_or_else(|| anyhow::anyhow!("empty cast file"))?,
    )?;
    let width = u16::try_from(header["width"].as_u64().unwrap_or(80)).unwrap_or(80);
    let height = u16::try_from(header["height"].as_u64().unwrap_or(24)).unwrap_or(24);
    let caps = if no_style {
        Caps::disabled()
    } else {
        Caps::detect_for_replay()
    };
    let config = Config::load();
    let mut transform = StylingTransform::new(&config, caps, height, width);
    let mut stdout = std::io::stdout().lock();
    let mut out = Vec::new();
    let mut last_t: Option<f64> = None;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (t, kind, data): (f64, String, String) = serde_json::from_str(line)?;
        if last_t.is_some_and(|last| t - last > REPLAY_QUIET_GAP_SECS) {
            out.clear();
            transform.note_virtual_quiet(&mut out);
            stdout.write_all(&out)?;
        }
        last_t = Some(t);
        match kind.as_str() {
            "o" => {
                out.clear();
                transform.transform(data.as_bytes(), &mut out);
                stdout.write_all(&out)?;
            }
            "r" => {
                if let Some((cols, rows)) = data.split_once('x')
                    && let (Ok(cols), Ok(rows)) = (cols.parse(), rows.parse())
                {
                    out.clear();
                    transform.resize(rows, cols, &mut out);
                    stdout.write_all(&out)?;
                }
            }
            _ => {}
        }
    }
    out.clear();
    transform.finish(&mut out);
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(())
}
