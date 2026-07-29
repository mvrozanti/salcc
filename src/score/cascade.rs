//! Cascade merge of heuristic and LLM span scores over the same clean text.
//! Contract:
//! - Overlapping spans of the SAME facet: max weight wins, ties go to the
//!   LLM span (it saw the whole block; the heuristic saw one line).
//! - An LLM span with weight 0 is a veto request against every heuristic
//!   span it overlaps; it is honored only where the VetoPolicy allows the
//!   overlapped heuristic facet and it never appears in the output itself.
//!   Risk is NEVER vetoable (CLAUDE.md invariant 7) — VetoPolicy hard-codes
//!   it, no configuration can change it.
//! - Overlaps across different facets both pass through; the emission
//!   styler owns cross-facet overlap arbitration and the density budget.
//! - Non-overlapping spans pass through untouched.
//! - Output order is deterministic: range start, then range end, then
//!   heuristic before LLM.

use crate::config::VetoPolicy;
use crate::types::{SourceId, SpanScore};

pub fn merge(heuristic: &[SpanScore], llm: &[SpanScore], policy: &VetoPolicy) -> Vec<SpanScore> {
    let (vetoes, llm_scores): (Vec<&SpanScore>, Vec<&SpanScore>) =
        llm.iter().partition(|span| span.weight == 0.0);
    let survivors: Vec<&SpanScore> = heuristic
        .iter()
        .filter(|h| !is_vetoed(h, &vetoes, policy))
        .filter(|h| {
            !llm_scores
                .iter()
                .any(|l| l.facet == h.facet && overlaps(l, h) && l.weight >= h.weight)
        })
        .collect();
    let mut merged: Vec<SpanScore> = survivors.iter().map(|span| (*span).clone()).collect();
    merged.extend(
        llm_scores
            .iter()
            .filter(|l| {
                !survivors
                    .iter()
                    .any(|h| h.facet == l.facet && overlaps(h, l) && h.weight > l.weight)
            })
            .map(|span| (*span).clone()),
    );
    merged.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.range.end.cmp(&b.range.end))
            .then(source_rank(a.source).cmp(&source_rank(b.source)))
    });
    merged
}

fn is_vetoed(span: &SpanScore, vetoes: &[&SpanScore], policy: &VetoPolicy) -> bool {
    let veto_requested = vetoes.iter().any(|veto| overlaps(veto, span));
    if !veto_requested {
        return false;
    }
    if !policy.allows(&span.facet) {
        tracing::debug!(
            facet = ?span.facet,
            range = ?span.range,
            "llm veto request ignored: policy forbids vetoing this facet"
        );
        return false;
    }
    tracing::debug!(facet = ?span.facet, range = ?span.range, "heuristic span vetoed by llm");
    true
}

fn overlaps(a: &SpanScore, b: &SpanScore) -> bool {
    a.range.start < b.range.end && b.range.start < a.range.end
}

fn source_rank(source: SourceId) -> u8 {
    match source {
        SourceId::Heuristic => 0,
        SourceId::Anthropic => 1,
        SourceId::Ollama => 2,
    }
}
