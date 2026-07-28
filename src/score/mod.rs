//! Pluggable scorers producing SpanScores over clean text.
//!
//! - heuristic: sync, instant, runs at emission. Every rule carries a
//!   precision annotation; rules below 0.85 estimated precision must be
//!   dim-only or disabled by default (reliability floor: below ~0.70,
//!   highlighting is worse than nothing — Wickens & Dixon 2007).
//! - anthropic: default async scorer, claude-haiku-4-5, structured output,
//!   spans as verbatim quotes (never offsets — models hallucinate offsets).
//!   Sparse marking by prompt: precision over recall, empty list is valid.
//! - cascade: merges heuristic + LLM spans. Max weight per facet per
//!   overlapping region. LLM may veto a heuristic span only where configured
//!   (llm_can_veto) — NEVER for Risk.
//! - cache: keyed by blake3(normalized block text); also (message_id, block).
//!
//! Cost discipline: skip blocks < min_block_words, coalesce blocks completing
//! within 500ms, hard-stop at per-session USD budget, log every API call to
//! stats metrics (ts, tokens, usd, spans, cache_hit).

pub mod anthropic;
pub mod cache;
pub mod cascade;
pub mod heuristic;
