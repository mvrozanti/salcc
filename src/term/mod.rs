//! Shadow terminal state. One truth: the vt100 grid fed every child byte,
//! plus a second cheap vte pass (telemetry) observing raw events vt100 hides
//! (cursor targets, scrolls, erases, live SGR state, alt-screen).
//!
//! finalize.rs is the correctness linchpin of the whole project: it decides
//! which printed lines are scrollback-bound (safe to style exactly once at
//! emission) vs inside the child's actively repainted region (must pass
//! through untouched or styling flickers/corrupts). A false ScrollbackBound
//! that the child later rewrites must invalidate our overlay — the child
//! always wins.
//!
//! repaint.rs may only ever target rows that are: visible AND final AND
//! text-unchanged since scoring. Never rows >= the dynamic-region top.

pub mod caps;
pub mod finalize;
pub mod grid;
pub mod repaint;
pub mod telemetry;
