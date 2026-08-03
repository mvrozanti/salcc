//! CLI surface. `salcc <cmd...>` wraps a child; subcommands are reserved
//! words that cannot collide with wrapping
//! (record/replay/stats/doctor/completions).
//! Transform selection: `--no-style`, `NO_COLOR`, a non-tty stdout, or an
//! unknown terminal select the identity transform (bit-perfect passthrough);
//! otherwise the styling pipeline runs, and when the configured scorer is
//! "self" (default) or "anthropic" the async scoring side channel is
//! spawned before the child (SystemTime captured pre-spawn so session
//! discovery can watch for the new transcript) and attached to the
//! transform. A missing API key logs one warning and degrades to
//! heuristics-only — never a crash, never a block. For scorer "self" with
//! `scorer.inject_instruction` (default true) and a child whose basename is
//! "claude", a one-shot `--help` probe (2s budget, SIGKILL on timeout)
//! checks for `--append-system-prompt`; if advertised, the self-annotation
//! instruction is appended to the child argv, otherwise a warning logs and
//! doctor explains the manual setup. `record` tees raw child output into an asciinema cast v2 file
//! while passing through untouched; `replay` runs a cast through the
//! styling transform offline (cast gaps drive burst ends, verdicts settle
//! at EOF) and writes the styled bytes to stdout — the demo and debugging
//! tool for the emission channel. The proxy also registers the SIGHUP
//! config-reload flag the transform polls at its flush tick. `stats`
//! prints the metrics.jsonl cost summary as a plain table, or with
//! --serve hosts the localhost dashboard on a blocking current-thread
//! runtime (stats never wraps a child, so owning the terminal is fine).
//! `doctor` (term/doctor.rs) reports capability detection, config parse
//! status, key resolution, transcript discovery, and metrics state;
//! `completions` emits clap_complete output for zsh/bash/fish to stdout.
//! Both own the terminal — no child — so printing to stdout is correct.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime};

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::emit::pipeline::{CacheHitLog, SessionSource, StylingTransform, spawn_side_channel};
use crate::proxy::io_loop::{self, ChunkTransform, IdentityTransform};
use crate::proxy::pty;
use crate::proxy::signals;
use crate::score::anthropic::AnthropicScorer;
use crate::score::self_report::{SELF_ANNOTATION_INSTRUCTION, SelfScorer};
use crate::stats::metrics::MetricsLog;
use crate::stats::server;
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
    /// Emit shell completions to stdout
    Completions {
        shell: clap_complete::Shell,
    },
}

pub fn run(args: Cli) -> anyhow::Result<()> {
    match args.command {
        Some(Command::Record { output, child }) => run_record(&output, &child),
        Some(Command::Replay { fixture }) => run_replay(&fixture, args.no_style),
        Some(Command::Stats { serve, port }) => run_stats(serve, port),
        Some(Command::Doctor { verify }) => crate::term::doctor::run(verify),
        Some(Command::Completions { shell }) => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut cmd, "salcc", &mut script);
            match std::io::stdout().write_all(&script) {
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                other => Ok(other?),
            }
        }
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
    transform.watch_reload_flag(signals::register_reload_flag());
    if matches!(config.scorer.name.as_str(), "self" | "anthropic") {
        attach_score_side_channel(&config, &mut transform);
    }
    let child = maybe_inject_instruction(&config, child);
    io_loop::run(&child, &mut transform)
}

fn attach_score_side_channel(config: &Config, transform: &mut StylingTransform) {
    let (scorer, cache_log): (Box<dyn crate::types::Scorer>, Option<CacheHitLog>) =
        if config.scorer.name == "self" {
            (Box::new(SelfScorer::new(config.dimensions.clone())), None)
        } else {
            let metrics = MetricsLog::open_default();
            let cache_log = CacheHitLog {
                metrics: metrics.clone(),
                model: config.scorer.model.clone(),
            };
            match AnthropicScorer::new(&config.scorer, config.dimensions.clone(), metrics) {
                Ok(scorer) => (Box::new(scorer), Some(cache_log)),
                Err(err) => {
                    tracing::warn!(%err, "anthropic scorer unavailable; styling continues heuristics-only");
                    return;
                }
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
    transform.attach_score_channel(spawn_side_channel(source, scorer, cache_log));
}

const INJECTION_FLAG: &str = "--append-system-prompt";
const HELP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn maybe_inject_instruction(config: &Config, child: &[String]) -> Vec<String> {
    let mut argv = child.to_vec();
    if config.scorer.name != "self" || !config.scorer.inject_instruction {
        return argv;
    }
    let Some(argv0) = child.first() else {
        return argv;
    };
    let basename = std::path::Path::new(argv0)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if basename != "claude" {
        tracing::debug!(%basename, "self-annotation instruction not injected: child is not claude");
        return argv;
    }
    if !help_mentions_flag(argv0) {
        tracing::warn!(
            %argv0,
            "child --help does not advertise {INJECTION_FLAG}; self-annotation instruction not injected — add it to the agent's system prompt manually (see salcc doctor)"
        );
        return argv;
    }
    argv.push(INJECTION_FLAG.to_string());
    argv.push(SELF_ANNOTATION_INSTRUCTION.to_string());
    tracing::info!("self-annotation instruction injected via {INJECTION_FLAG}");
    argv
}

fn help_mentions_flag(argv0: &str) -> bool {
    let spawned = std::process::Command::new(argv0)
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let child = match spawned {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(%err, %argv0, "help probe failed to spawn");
            return false;
        }
    };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tx.send(child.wait_with_output()).ok();
    });
    match rx.recv_timeout(HELP_PROBE_TIMEOUT) {
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stdout).contains(INJECTION_FLAG),
        Ok(Err(err)) => {
            tracing::warn!(%err, "help probe failed to run");
            false
        }
        Err(_) => {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            tracing::warn!(timeout = ?HELP_PROBE_TIMEOUT, "help probe timed out; killed");
            false
        }
    }
}

fn run_stats(serve: bool, port: u16) -> anyhow::Result<()> {
    io_loop::init_tracing();
    let log = MetricsLog::open_default();
    if !serve {
        print!("{}", server::render_table(&log.read_all()));
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let bound = server::bind(log, port).await?;
        println!("salcc stats dashboard: http://{}", bound.addr);
        bound.run().await
    })
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
