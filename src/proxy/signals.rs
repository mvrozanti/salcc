//! Signal thread for the proxy. SIGWINCH re-reads the real terminal size and
//! resizes the child pty (the kernel then delivers SIGWINCH to the child).
//! SIGTERM forwards to the child so the main loop reaps it and restores the
//! terminal. SIGINT forwards only when the real terminal is not raw: in raw
//! mode Ctrl-C reaches the child as a byte through the pty and salcc cannot
//! receive keyboard SIGINT, so forwarding would double-signal. SIGTSTP
//! restores termios and stops the whole process; SIGCONT re-enters raw mode
//! and re-syncs the pty size in case the terminal changed while stopped.
//! SIGHUP is a config-reload request: `register_reload_flag` raises an
//! AtomicBool the styling transform polls at its flush tick (mirroring the
//! SIGWINCH flag pattern in io_loop) and Config::load() runs on that tick;
//! registering the handler also stops SIGHUP from killing the proxy,
//! matching daemon reload semantics.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, PoisonError};
use std::thread;

use anyhow::{Context, Result};
use rustix::process::{Pid, Signal, getpid, kill_process};
use signal_hook::consts::signal::{SIGCONT, SIGHUP, SIGINT, SIGTERM, SIGTSTP, SIGWINCH};
use signal_hook::iterator::Signals;

use super::pty::{self, SharedMaster};

pub fn spawn_signal_thread(
    master: SharedMaster,
    child_pid: Option<u32>,
    raw_tty: bool,
) -> Result<()> {
    let mut signals = Signals::new([SIGWINCH, SIGINT, SIGTERM, SIGTSTP, SIGCONT])
        .context("signal handler registration failed")?;
    thread::Builder::new()
        .name("salcc-signals".into())
        .spawn(move || {
            for sig in signals.forever() {
                tracing::debug!(sig, "signal received");
                match sig {
                    SIGWINCH => resize_child_pty(&master),
                    SIGTERM => signal_child(child_pid, Signal::TERM),
                    SIGINT if !raw_tty => signal_child(child_pid, Signal::INT),
                    SIGINT => {}
                    SIGTSTP => suspend_self(),
                    SIGCONT => {
                        pty::reenter_raw_mode();
                        resize_child_pty(&master);
                    }
                    _ => {}
                }
            }
        })
        .context("signal thread spawn failed")?;
    Ok(())
}

pub fn register_reload_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    match signal_hook::flag::register(SIGHUP, flag.clone()) {
        Ok(_) => tracing::debug!("SIGHUP config-reload flag registered"),
        Err(err) => {
            tracing::warn!(%err, "SIGHUP registration failed; config reload unavailable this session");
        }
    }
    flag
}

fn resize_child_pty(master: &SharedMaster) {
    let size = pty::real_terminal_size();
    let guard = master.lock().unwrap_or_else(PoisonError::into_inner);
    match guard.resize(size) {
        Ok(()) => tracing::debug!(?size, "child pty resized"),
        Err(err) => tracing::warn!(%err, "child pty resize failed"),
    }
}

fn signal_child(child_pid: Option<u32>, sig: Signal) {
    let Some(pid) = child_pid.and_then(|p| i32::try_from(p).ok()).and_then(Pid::from_raw) else {
        tracing::warn!(?sig, "no child pid available; signal not forwarded");
        return;
    };
    match kill_process(pid, sig) {
        Ok(()) => tracing::debug!(?sig, pid = pid.as_raw_nonzero().get(), "signal forwarded to child"),
        Err(err) => tracing::warn!(%err, ?sig, "signal forward to child failed"),
    }
}

fn suspend_self() {
    pty::restore_termios();
    tracing::debug!("SIGTSTP: termios restored, stopping self");
    let _ = kill_process(getpid(), Signal::STOP);
}
