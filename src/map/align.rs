//! Whole-message anchor: byte-exact memmem search of the normalized clean
//! text inside the normalized grid window (the window may contain unrelated
//! surrounding rows). Returns the offset only for a UNIQUE match; absent or
//! multiply-occurring clean text yields None so the per-span fallback (and
//! ultimately the fail-safe drop, invariant 4) takes over — never guess
//! between equal candidates.

use memchr::memmem;

pub fn anchor(clean_norm: &str, grid_norm: &str) -> Option<usize> {
    if clean_norm.is_empty() {
        return None;
    }
    let mut hits = memmem::find_iter(grid_norm.as_bytes(), clean_norm.as_bytes());
    let first = hits.next()?;
    if hits.next().is_some() {
        tracing::debug!(
            len = clean_norm.len(),
            "anchor ambiguous: clean text occurs more than once in grid window"
        );
        return None;
    }
    Some(first)
}
