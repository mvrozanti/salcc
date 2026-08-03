//! Self-annotation scorer (default). The wrapped agent is instructed (via
//! `--append-system-prompt`, injected in cli.rs when supported) to end each
//! response with one machine-consumed line like
//! `⟦r:verbatim quote❙a:another quote⟧`. This module parses those regions
//! out of transcript block text, resolves each quote to a byte range via
//! memmem against the annotation-blanked prose (fail-safe: unresolvable or
//! ambiguous-shaped entries drop), and emits `SpanScore`s with
//! `SourceId::SelfReport` and fixed per-facet weights — zero API calls,
//! zero added latency. Blanking replaces every byte of an annotation region
//! with a space, so byte offsets into the blanked string remain valid into
//! the original text. Facet codes: r/a/c/u/n map to the builtin facets;
//! any other code becomes `Facet::User(code)` (full dimension names).
//! Cross-block resolution (`resolve_against_message`) handles annotations
//! that quote an earlier block of the same message: current block first,
//! then earlier blocks most-recent-first; the span is attributed to the
//! block whose text contained the quote. On screen the annotation line is
//! never removed — the emission styler conceals it (SGR 8 + 2, attrs-only;
//! `is_annotation_line` is the shared detector).

use smol_str::SmolStr;

use crate::config::dimensions::UserDimension;
use crate::score::anthropic::resolve_quote;
use crate::types::{Facet, LatencyClass, ScoreFuture, ScoreRequest, Scorer, SourceId, SpanScore};

pub const ANNOTATION_OPEN: char = '⟦';
pub const ANNOTATION_CLOSE: char = '⟧';
pub const ANNOTATION_SEP: char = '❙';

pub const RISK_WEIGHT: f32 = 0.9;
pub const ACTION_WEIGHT: f32 = 0.7;
pub const COMMAND_WEIGHT: f32 = 0.6;
pub const UNCERTAINTY_WEIGHT: f32 = 0.5;
pub const NOVELTY_WEIGHT: f32 = 0.5;
pub const USER_WEIGHT: f32 = 0.7;

pub const QUOTE_MIN_CHARS: usize = 3;
pub const QUOTE_MAX_CHARS: usize = 80;

pub const SELF_ANNOTATION_INSTRUCTION: &str = "At the very end of each response, you may append ONE line marking spans a busy expert must not miss, exactly this format: ⟦r:verbatim quote❙a:another verbatim quote⟧ — facet codes: r=destructive/irreversible/security risk, a=action the user must do themselves, c=runnable command worth copying, u=hedged/unverified claim, n=first mention of a new file/decision/constraint. Each value is a VERBATIM substring (3-80 chars) copied exactly from your response above. Be sparse: 0-3 marks per response, most responses deserve none — omit the line entirely then. Never mark boilerplate, headings, or code-fence markers. The line is machine-consumed and hidden from the user.";

pub fn facet_for_code(code: &str) -> Facet {
    match code {
        "r" => Facet::Risk,
        "a" => Facet::Action,
        "c" => Facet::Command,
        "u" => Facet::Uncertainty,
        "n" => Facet::Novelty,
        other => Facet::User(SmolStr::from(other)),
    }
}

pub fn facet_weight(facet: &Facet) -> f32 {
    match facet {
        Facet::Risk => RISK_WEIGHT,
        Facet::Action => ACTION_WEIGHT,
        Facet::Command => COMMAND_WEIGHT,
        Facet::Uncertainty => UNCERTAINTY_WEIGHT,
        Facet::Novelty => NOVELTY_WEIGHT,
        Facet::Dim => 0.0,
        Facet::User(_) => USER_WEIGHT,
    }
}

pub fn is_annotation_line(clean: &str) -> bool {
    let trimmed = clean.trim();
    let Some(inner) = trimmed
        .strip_prefix(ANNOTATION_OPEN)
        .and_then(|rest| rest.strip_suffix(ANNOTATION_CLOSE))
    else {
        return false;
    };
    !inner.contains(ANNOTATION_OPEN) && !inner.contains(ANNOTATION_CLOSE)
}

pub fn extract_annotations(text: &str) -> (String, Vec<(Facet, String)>) {
    let mut blanked = text.as_bytes().to_vec();
    let mut entries = Vec::new();
    let mut cursor = 0;
    while let Some(open_rel) = text[cursor..].find(ANNOTATION_OPEN) {
        let open = cursor + open_rel;
        let inner_start = open + ANNOTATION_OPEN.len_utf8();
        let Some(close_rel) = text[inner_start..].find(ANNOTATION_CLOSE) else {
            break;
        };
        let close = inner_start + close_rel;
        for entry in text[inner_start..close].split(ANNOTATION_SEP) {
            match parse_entry(entry) {
                Some(parsed) => entries.push(parsed),
                None => tracing::debug!(entry, "malformed self-annotation entry skipped"),
            }
        }
        let end = close + ANNOTATION_CLOSE.len_utf8();
        blanked[open..end].fill(b' ');
        cursor = end;
    }
    let blanked = String::from_utf8(blanked)
        .expect("blanking replaces whole annotation chars with ascii spaces");
    (blanked, entries)
}

fn parse_entry(entry: &str) -> Option<(Facet, String)> {
    let (code, quote) = entry.split_once(':')?;
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    let chars = quote.trim().chars().count();
    if !(QUOTE_MIN_CHARS..=QUOTE_MAX_CHARS).contains(&chars) {
        return None;
    }
    Some((facet_for_code(code), quote.to_string()))
}

pub fn resolve_against_message(
    blanked_blocks: &[String],
    entries: &[(Facet, String)],
) -> Vec<(usize, SpanScore)> {
    let mut resolved = Vec::new();
    let Some(last) = blanked_blocks.len().checked_sub(1) else {
        return resolved;
    };
    for (facet, quote) in entries {
        let hit = std::iter::once(last)
            .chain((0..last).rev())
            .find_map(|idx| resolve_quote(&blanked_blocks[idx], quote, 1).map(|range| (idx, range)));
        match hit {
            Some((idx, range)) => {
                if resolve_quote(&blanked_blocks[idx], quote, 2).is_some() {
                    tracing::debug!(%quote, block = idx, "self-annotation quote ambiguous; first occurrence styled");
                }
                resolved.push((
                    idx,
                    SpanScore::new(range, facet.clone(), facet_weight(facet), SourceId::SelfReport),
                ));
            }
            None => tracing::debug!(%quote, "self-annotation quote not found in message; span dropped"),
        }
    }
    resolved
}

pub struct SelfScorer {
    dimensions: Vec<UserDimension>,
}

impl SelfScorer {
    pub fn new(dimensions: Vec<UserDimension>) -> Self {
        Self { dimensions }
    }

    fn note_unconfigured_dimensions(&self, entries: &[(Facet, String)]) {
        for (facet, _) in entries {
            if let Facet::User(name) = facet
                && !self.dimensions.iter().any(|dim| dim.name == *name)
            {
                tracing::debug!(%name, "self-annotation uses a user dimension not in config; default styling applies");
            }
        }
    }
}

impl Scorer for SelfScorer {
    fn name(&self) -> &'static str {
        "self"
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Instant
    }

    fn score<'a>(&'a self, req: ScoreRequest<'a>) -> ScoreFuture<'a> {
        let (blanked, entries) = extract_annotations(req.text);
        self.note_unconfigured_dimensions(&entries);
        let spans = resolve_against_message(std::slice::from_ref(&blanked), &entries)
            .into_iter()
            .map(|(_, span)| span)
            .collect();
        Box::pin(std::future::ready(Ok(spans)))
    }
}
