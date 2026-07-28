//! Emission-time styling: the only chance to style text before it scrolls
//! into unreachable scrollback.
//!
//! Pipeline: segment child chunks into TEXT runs (printable + SGR + CR/LF)
//! and CTRL runs (everything else). CTRL runs flush the hold and pass through
//! unmodified, order preserved. Complete TEXT lines wait in the hold buffer
//! (deadline: config.hold_ms, default 150ms) for a finality verdict, then get
//! styled (heuristics + any LLM spans already cached) and emitted.
//!
//! Hard invariants:
//! - Styling changes cell ATTRIBUTES only — never text, never width. Sole
//!   audited exception: a facet glyph may replace a 2-space gutter pad.
//! - Output always terminates restoring the child's live SGR state.
//! - Density budget: emphasized (non-Dim) cells > config.density_budget of a
//!   block => drop lowest-weight spans until under budget.
//! - NO_COLOR / --no-style / alt-screen active => bit-perfect passthrough.

pub mod hold;
pub mod segment;
pub mod styler;
