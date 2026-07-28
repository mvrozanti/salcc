//! PTY proxy: the passthrough hot path. Spawns the child in a PTY, forwards
//! stdin raw and immediately (NEVER held, NEVER transformed), pipes child
//! output through the styling pipeline to the real tty.
//!
//! Hard invariants:
//! - stdin path: zero buffering, zero inspection.
//! - termios raw-mode restore is guaranteed on every exit path including
//!   panic (scopeguard + panic hook).
//! - child exit code propagates exactly; signals (SIGWINCH, SIGINT, SIGTERM)
//!   forward correctly; Ctrl-Z/fg must survive.
//! - p99 chunk-in -> chunk-out latency < 1ms (benches/passthrough.rs).

pub mod io_loop;
pub mod pty;
pub mod signals;
