//! Passthrough hot path and proxy orchestration. Real stdin forwards to the
//! child pty on a dedicated thread with zero buffering and zero inspection.
//! Child output flows pty reader thread -> mpsc -> ChunkTransform -> real
//! stdout on the calling thread. The recv ticks every 50ms so a styling
//! transform can flush its hold on quiet (`flush`), SIGWINCH is mirrored to
//! the transform via a flag the pump polls (`resize`), and channel
//! disconnect drains the transform (`finish`) so held bytes are never lost
//! at child exit. IdentityTransform keeps every hook a no-op, preserving M0
//! byte-perfect passthrough. EIO from the pty reader is normal child exit,
//! not an error. Grandchildren holding the pty open keep the session alive
//! until output EOF, matching script(1) semantics. The child exit code is
//! returned for exact propagation by the caller.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use rustix::fd::BorrowedFd;
use rustix::io::Errno;
use tracing_subscriber::EnvFilter;

use super::{pty, signals};

const CHUNK_CAPACITY: usize = 64 * 1024;
const TICK: Duration = Duration::from_millis(50);

pub trait ChunkTransform {
    fn transform(&mut self, chunk: &[u8], out: &mut Vec<u8>);

    fn flush(&mut self, out: &mut Vec<u8>) {
        let _ = out;
    }

    fn resize(&mut self, rows: u16, cols: u16, out: &mut Vec<u8>) {
        let _ = (rows, cols, out);
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        self.flush(out);
    }
}

pub struct IdentityTransform;

impl ChunkTransform for IdentityTransform {
    fn transform(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(chunk);
    }
}

pub fn run(argv: &[String], transform: &mut dyn ChunkTransform) -> Result<i32> {
    init_tracing();
    pty::install_termios_panic_hook();
    let spawned = pty::spawn_child(argv, pty::real_terminal_size())?;
    let pty::SpawnedChild {
        master,
        reader,
        writer,
        mut child,
    } = spawned;
    let raw_tty = pty::enter_raw_mode()?;
    let _restore = scopeguard::guard((), |()| pty::restore_termios_final());
    signals::spawn_signal_thread(master.clone(), child.process_id(), raw_tty)?;
    let winch = Arc::new(AtomicBool::new(false));
    if let Err(err) = signal_hook::flag::register(signal_hook::consts::SIGWINCH, winch.clone()) {
        tracing::warn!(%err, "SIGWINCH flag registration failed; transform will not learn of resizes");
    }
    spawn_stdin_pump(writer);
    let chunks = spawn_output_reader(reader);
    let stdout_alive = pump_output(&chunks, transform, &winch);
    if !stdout_alive {
        tracing::warn!("real stdout closed before child exit; killing child");
        let _ = child.kill();
    }
    let status = child.wait().context("child wait failed")?;
    drop(master);
    tracing::info!(code = status.exit_code(), signal = ?status.signal(), "child exited");
    Ok(i32::try_from(status.exit_code()).unwrap_or(1))
}

fn spawn_stdin_pump(mut writer: Box<dyn Write + Send>) {
    let spawned = thread::Builder::new()
        .name("salcc-stdin".into())
        .spawn(move || {
            let fd = rustix::stdio::stdin();
            let mut buf = vec![0u8; CHUNK_CAPACITY];
            loop {
                match rustix::io::read(fd, &mut *buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer
                            .write_all(&buf[..n])
                            .and_then(|()| writer.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(Errno::INTR) => {}
                    Err(err) => {
                        tracing::debug!(%err, "stdin read ended");
                        break;
                    }
                }
            }
            tracing::debug!("stdin pump exited");
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "stdin pump thread failed to spawn; keystrokes will not reach child");
    }
}

fn spawn_output_reader(mut reader: Box<dyn Read + Send>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("salcc-pty-read".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_CAPACITY];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(err) => {
                        tracing::debug!(%err, "pty reader ended (EIO here means child exit)");
                        break;
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "pty reader thread failed to spawn; no output will flow");
    }
    rx
}

fn pump_output(
    chunks: &Receiver<Vec<u8>>,
    transform: &mut dyn ChunkTransform,
    winch: &AtomicBool,
) -> bool {
    let fd = rustix::stdio::stdout();
    let mut styled = Vec::with_capacity(CHUNK_CAPACITY);
    loop {
        styled.clear();
        apply_pending_resize(winch, transform, &mut styled);
        match chunks.recv_timeout(TICK) {
            Ok(chunk) => transform.transform(&chunk, &mut styled),
            Err(RecvTimeoutError::Timeout) => transform.flush(&mut styled),
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Err(err) = write_all_fd(fd, &styled) {
            tracing::warn!(%err, "real stdout write failed");
            return false;
        }
    }
    styled.clear();
    transform.finish(&mut styled);
    if let Err(err) = write_all_fd(fd, &styled) {
        tracing::warn!(%err, "real stdout write failed during final drain");
        return false;
    }
    true
}

fn apply_pending_resize(winch: &AtomicBool, transform: &mut dyn ChunkTransform, out: &mut Vec<u8>) {
    if winch.swap(false, Ordering::SeqCst) {
        let size = pty::real_terminal_size();
        if size.rows > 0 && size.cols > 0 {
            transform.resize(size.rows, size.cols, out);
        }
    }
}

fn write_all_fd(fd: BorrowedFd, buf: &[u8]) -> rustix::io::Result<()> {
    let mut rest = buf;
    while !rest.is_empty() {
        match rustix::io::write(fd, rest) {
            Ok(0) => return Err(Errno::IO),
            Ok(n) => rest = &rest[n..],
            Err(Errno::INTR) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn init_tracing() {
    let preferred = dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("salcc");
    let dir = if std::fs::create_dir_all(&preferred).is_ok() {
        preferred
    } else {
        let fallback = PathBuf::from("/tmp/salcc");
        if std::fs::create_dir_all(&fallback).is_err() {
            return;
        }
        fallback
    };
    let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("salcc.log"))
    else {
        return;
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .try_init()
        .ok();
}
