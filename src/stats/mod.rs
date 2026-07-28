//! Cost observability. metrics.rs appends one JSONL record per LLM scoring
//! call to $XDG_STATE_HOME/salcc/metrics.jsonl:
//! {ts, session, block_id, model, input_tokens, output_tokens, usd,
//!  spans_returned, cache_hit}. usd derives from the API usage response
//! fields, never estimated client-side.
//!
//! server.rs: `salcc stats --serve` — axum on 127.0.0.1:7877, single embedded
//! HTML page (include_str!, no build step), JSON endpoints /api/summary,
//! /api/sessions, /api/session/:id. Reads metrics.jsonl only; no live
//! coupling to the proxy. `salcc stats` prints the same summary as a table.

pub mod metrics;
pub mod server;
