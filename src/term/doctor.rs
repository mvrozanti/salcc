//! `salcc doctor`: plain aligned-text diagnosis of everything the proxy
//! resolves at spawn. Sections: terminal capability detection (raw env vars
//! plus the derived Caps, using the exact same detect path as the live
//! proxy), config file path + parse status (parse errors surface verbatim
//! here — this is where users debug their YAML; the proxy itself only logs
//! and falls back to defaults), scorer identity (for "self": whether the
//! launch-time instruction injection is enabled plus the manual-setup hint;
//! for "anthropic": API key resolution status — env var presence, cmd
//! configured, live resolution outcome, the key itself is never printed),
//! transcript discovery for the current cwd
//! (project dir, newest session file or none), and the metrics log location
//! and record count. `--verify` additionally spawns `salcc cat` on a temp
//! file inside a real PTY (same rig as tests/proxy_integration.rs) and
//! checks byte-identity of the round trip; the payload is a single line
//! with no newline so PTY ONLCR translation cannot alter it, and an
//! incomplete line always flushes as original bytes, making the check
//! deterministic under any TERM. Doctor owns the terminal (no child), so
//! printing to stdout is correct here. Verify failure returns Err so the
//! process exits nonzero.

use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::config::Config;
use crate::score::anthropic::resolve_api_key;
use crate::stats::metrics::MetricsLog;
use crate::tail::discover;
use crate::term::caps::{Caps, ColorDepth};

const VERIFY_BUDGET: Duration = Duration::from_secs(10);

pub fn run(verify: bool) -> anyhow::Result<()> {
    let mut out = String::new();
    render_terminal(&mut out);
    render_config(&mut out);
    render_transcripts(&mut out);
    render_metrics(&mut out);
    let passed = if verify { render_verify(&mut out) } else { true };
    print!("{out}");
    if !passed {
        anyhow::bail!("doctor --verify failed");
    }
    Ok(())
}

fn line(out: &mut String, label: &str, value: impl AsRef<str>) {
    let _ = writeln!(out, "  {:<18} {}", label, value.as_ref());
}

fn env_or_unset(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "(unset)".to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn render_terminal(out: &mut String) {
    let term = std::env::var("TERM").ok();
    let colorterm = std::env::var("COLORTERM").ok();
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let tty = rustix::termios::isatty(rustix::stdio::stdout());
    let caps = Caps::from_env_parts(
        term.as_deref(),
        colorterm.as_deref(),
        term_program.as_deref(),
        no_color,
        tty,
    );
    let _ = writeln!(out, "terminal");
    line(out, "TERM", env_or_unset("TERM"));
    line(out, "COLORTERM", env_or_unset("COLORTERM"));
    line(out, "TERM_PROGRAM", env_or_unset("TERM_PROGRAM"));
    line(out, "NO_COLOR", if no_color { "set" } else { "unset" });
    line(out, "stdout", if tty { "tty" } else { "not a tty" });
    line(
        out,
        "styling",
        if caps.styling_enabled {
            "enabled"
        } else {
            "disabled (bit-perfect passthrough)"
        },
    );
    let depth = match caps.depth {
        ColorDepth::TrueColor => "truecolor",
        ColorDepth::C256 => "256",
        ColorDepth::C16 => "16",
    };
    line(out, "color depth", depth);
    line(out, "undercurl", yes_no(caps.undercurl));
    line(out, "sync-update", yes_no(caps.sync_update));
    let _ = writeln!(out);
}

fn render_config(out: &mut String) {
    let _ = writeln!(out, "config");
    let path = Config::path();
    let (config, status) = match &path {
        None => (Config::default(), "no config dir (defaults active)".to_string()),
        Some(path) => match std::fs::read_to_string(path) {
            Err(_) => (Config::default(), "not found (defaults active)".to_string()),
            Ok(text) => match Config::from_yaml(&text) {
                Ok(config) => (config, "parsed ok".to_string()),
                Err(err) => (
                    Config::default(),
                    format!("PARSE ERROR: {err} (defaults active)"),
                ),
            },
        },
    };
    line(
        out,
        "path",
        path.as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no config dir)".to_string()),
    );
    line(out, "status", status);
    line(out, "scorer", &config.scorer.name);
    if config.scorer.name == "self" {
        let injection = if config.scorer.inject_instruction {
            "enabled (--append-system-prompt injected at launch when supported; \
             add the self-annotation instruction to your agent's system prompt otherwise)"
        } else {
            "disabled (add the self-annotation instruction to your agent's system prompt manually)"
        };
        line(out, "injection", injection);
    } else if config.scorer.name == "anthropic" {
        line(out, "model", &config.scorer.model);
        let env_set = std::env::var_os(&config.scorer.api_key_env).is_some();
        line(
            out,
            "api key env",
            format!(
                "{} ({})",
                config.scorer.api_key_env,
                if env_set { "set" } else { "not set" }
            ),
        );
        line(
            out,
            "api key cmd",
            match &config.scorer.api_key_cmd {
                Some(cmd) => format!("configured: {cmd}"),
                None => "(not configured)".to_string(),
            },
        );
        let resolution = match resolve_api_key(&config.scorer) {
            Ok(_) => "ok".to_string(),
            Err(err) => format!("unresolved: {err} (styling degrades to heuristics-only)"),
        };
        line(out, "key resolution", resolution);
    }
    let _ = writeln!(out);
}

fn render_transcripts(out: &mut String) {
    let _ = writeln!(out, "transcripts");
    let Ok(cwd) = std::env::current_dir() else {
        line(out, "cwd", "(unavailable)");
        let _ = writeln!(out);
        return;
    };
    line(out, "cwd", cwd.display().to_string());
    let project = discover::project_dir(&cwd);
    let exists = project.is_dir();
    line(
        out,
        "project dir",
        format!(
            "{} ({})",
            project.display(),
            if exists { "exists" } else { "missing" }
        ),
    );
    match discover::newest_jsonl(&project) {
        Some((path, modified)) => line(
            out,
            "newest session",
            format!("{} (modified {} ago)", path.display(), age(modified)),
        ),
        None => line(out, "newest session", "none"),
    }
    let _ = writeln!(out);
}

fn render_metrics(out: &mut String) {
    let _ = writeln!(out, "metrics");
    let log = MetricsLog::open_default();
    let exists = log.path().is_file();
    line(out, "path", log.path().display().to_string());
    let records = if exists {
        log.read_all().len().to_string()
    } else {
        "0 (file not created yet)".to_string()
    };
    line(out, "records", records);
    let _ = writeln!(out);
}

fn render_verify(out: &mut String) -> bool {
    let _ = writeln!(out, "self-check");
    let (verdict, passed) = match verify_pty_identity() {
        Ok(()) => ("PASS".to_string(), true),
        Err(err) => (format!("FAIL: {err}"), false),
    };
    line(out, "pty byte-identity", verdict);
    passed
}

fn age(modified: SystemTime) -> String {
    let Ok(elapsed) = modified.elapsed() else {
        return "a negative interval".to_string();
    };
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn verify_pty_identity() -> anyhow::Result<()> {
    let payload = format!(
        "salcc-doctor-selfcheck-{}-abcdefghijklmnopqrstuvwxyz-0123456789",
        std::process::id()
    );
    let path = std::env::temp_dir().join(format!("salcc-doctor-{}.txt", std::process::id()));
    std::fs::write(&path, &payload)?;
    let result = run_cat_in_pty(&path);
    let _ = std::fs::remove_file(&path);
    let output = result?;
    anyhow::ensure!(
        output == payload.as_bytes(),
        "output diverged from input: expected {} bytes, got {} ({:?})",
        payload.len(),
        output.len(),
        String::from_utf8_lossy(&output)
    );
    Ok(())
}

fn run_cat_in_pty(file: &Path) -> anyhow::Result<Vec<u8>> {
    let exe = std::env::current_exe()?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(exe);
    cmd.arg("cat");
    cmd.arg(file);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => collected.extend_from_slice(&buf[..n]),
            }
        }
        out_tx.send(collected).ok();
    });
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || {
        status_tx.send(child.wait()).ok();
    });
    let status = status_rx
        .recv_timeout(VERIFY_BUDGET)
        .map_err(|_| anyhow::anyhow!("salcc cat did not exit within {VERIFY_BUDGET:?}"))??;
    drop(pair.master);
    let output = out_rx
        .recv_timeout(VERIFY_BUDGET)
        .map_err(|_| anyhow::anyhow!("pty output collector hung"))?;
    anyhow::ensure!(
        status.exit_code() == 0,
        "salcc cat exited with code {}",
        status.exit_code()
    );
    Ok(output)
}
