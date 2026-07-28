//! Configuration: ~/.config/salcc/config.yaml (XDG), reloaded on SIGHUP.
//!
//! Covers scorer selection (default: anthropic/claude-haiku-4-5), hold_ms,
//! density_budget, per-facet toggles, theme overrides (Okabe-Ito defaults),
//! user-defined semantic dimensions (keywords -> aho-corasick rules + llm_hint
//! injected into the LLM prompt), per-command profiles (claude: transcript
//! tail + gutters; "*": grid-text scoring only).
//!
//! Evidence-locked defaults (do not change without reading docs/research.md):
//! dim-first on, density_budget 0.12, no blink ever, hues carry category
//! only, luminance carries importance.

pub mod dimensions;
pub mod theme;
