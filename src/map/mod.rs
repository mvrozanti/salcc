//! Span -> grid mapping: translate byte ranges scored on clean text into
//! terminal grid cell ranges.
//!
//! Algorithm: concatenate candidate finalized rows' cells with per-char
//! provenance (abs_row, col) -> strip agent gutter prefixes -> normalize both
//! sides (render-lite markdown stripping with source map; collapse whitespace
//! including wrap-inserted breaks) -> memmem whole-message anchor (child
//! wraps on spaces, so collapse makes grid text == clean text in the common
//! case) -> per-span quote search fallback with `occurrence` disambiguation.
//!
//! Hard invariant (fail-safe): an ambiguous or absent match DROPS the span.
//! Never style the wrong text.

pub mod align;
pub mod locate;
pub mod normalize;
