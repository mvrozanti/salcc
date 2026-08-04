//! Real-terminal state and child PTY lifecycle. Owns the saved termios and
//! every restore path: the io_loop scopeguard, the panic hook, and the
//! SIGTSTP/SIGCONT suspend cycle in signals.rs all funnel through here.
//! When salcc's stdout is not a tty the child pty is switched to raw so piped
//! output stays byte-identical; when stdout is a tty the child pty keeps
//! default cooked semantics and the real terminal goes raw instead.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rustix::fd::BorrowedFd;
use rustix::termios::{self, OptionalActions, Termios};

pub type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

static SAVED_TERMIOS: Mutex<Option<Termios>> = Mutex::new(None);
static RAW_DESIRED: AtomicBool = AtomicBool::new(false);

pub struct SpawnedChild {
    pub master: SharedMaster,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub fn real_terminal_size() -> PtySize {
    winsize_of(rustix::stdio::stdout())
        .or_else(|| winsize_of(rustix::stdio::stdin()))
        .unwrap_or_default()
}

fn winsize_of(fd: BorrowedFd) -> Option<PtySize> {
    let ws = termios::tcgetwinsize(fd).ok()?;
    if ws.ws_row == 0 || ws.ws_col == 0 {
        return None;
    }
    Some(PtySize {
        rows: ws.ws_row,
        cols: ws.ws_col,
        pixel_width: ws.ws_xpixel,
        pixel_height: ws.ws_ypixel,
    })
}

pub fn spawn_child(argv: &[String], size: PtySize) -> Result<SpawnedChild> {
    let pair = native_pty_system().openpty(size).context("openpty failed")?;
    if !termios::isatty(rustix::stdio::stdout()) {
        make_child_pty_raw(pair.master.as_ref());
    }
    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.env("SALCC_ACTIVE", "1");
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to spawn `{}`", argv.join(" ")))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .context("pty reader unavailable")?;
    let writer = pair
        .master
        .take_writer()
        .context("pty writer unavailable")?;
    tracing::info!(?size, pid = ?child.process_id(), ?argv, "child spawned in pty");
    Ok(SpawnedChild {
        master: Arc::new(Mutex::new(pair.master)),
        reader,
        writer,
        child,
    })
}

fn make_child_pty_raw(master: &dyn MasterPty) {
    let Some(raw_fd) = master.as_raw_fd() else {
        tracing::warn!("child pty exposes no fd; piped output may gain crlf mangling");
        return;
    };
    let fd = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    match termios::tcgetattr(fd) {
        Ok(mut attrs) => {
            attrs.make_raw();
            match termios::tcsetattr(fd, OptionalActions::Now, &attrs) {
                Ok(()) => tracing::debug!("child pty set raw for non-tty stdout"),
                Err(err) => tracing::warn!(%err, "tcsetattr raw on child pty failed"),
            }
        }
        Err(err) => tracing::warn!(%err, "tcgetattr on child pty failed"),
    }
}

pub fn enter_raw_mode() -> Result<bool> {
    let fd = rustix::stdio::stdin();
    if !termios::isatty(fd) {
        tracing::debug!("stdin is not a tty; proxying without raw mode");
        return Ok(false);
    }
    let orig = termios::tcgetattr(fd).context("tcgetattr on real stdin failed")?;
    let mut raw = orig.clone();
    raw.make_raw();
    *lock_saved() = Some(orig);
    RAW_DESIRED.store(true, Ordering::SeqCst);
    termios::tcsetattr(fd, OptionalActions::Now, &raw)
        .context("tcsetattr raw on real stdin failed")?;
    tracing::debug!("real terminal entered raw mode");
    Ok(true)
}

pub fn restore_termios() {
    if let Some(orig) = lock_saved().clone() {
        let _ = termios::tcsetattr(rustix::stdio::stdin(), OptionalActions::Now, &orig);
    }
}

pub fn restore_termios_final() {
    RAW_DESIRED.store(false, Ordering::SeqCst);
    restore_termios();
}

pub fn reenter_raw_mode() {
    if !RAW_DESIRED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(mut raw) = lock_saved().clone() {
        raw.make_raw();
        let _ = termios::tcsetattr(rustix::stdio::stdin(), OptionalActions::Now, &raw);
        tracing::debug!("real terminal re-entered raw mode after SIGCONT");
    }
}

pub fn install_termios_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_termios_final();
        prev(info);
    }));
}

fn lock_saved() -> MutexGuard<'static, Option<Termios>> {
    SAVED_TERMIOS.lock().unwrap_or_else(PoisonError::into_inner)
}
