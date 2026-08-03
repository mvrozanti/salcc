//! Instant heuristic scorer for the emission path. Every rule carries a
//! precision annotation; rules under the 0.85 floor (CLAUDE.md invariant 7)
//! are dim-only: UNCERTAINTY renders as de-emphasis and DIM is de-emphasis
//! by definition, so both are exempt from the floor that gates emphasis
//! facets. RISK targets near-1.0 recall on destructive-command shapes and is
//! never dropped by downstream budgets. Sparse by design: at most
//! MAX_SPANS_PER_LINE spans survive per line, highest weight first.
//! NOVELTY needs session entity state and stays off until M3. Lines that
//! are entirely a `⟦...⟧` self-annotation are never scored as content —
//! they belong to the conceal path in emit/styler.rs. Implements
//! the async `Scorer` trait by wrapping the synchronous `score_line`.

use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::Regex;

use crate::config::Config;
use crate::config::dimensions::{DimensionRules, word_bounded};
use crate::types::{Facet, LatencyClass, ScoreFuture, ScoreRequest, Scorer, SourceId, SpanScore};

pub const MAX_SPANS_PER_LINE: usize = 3;

pub const RISK_COMMAND_PRECISION: f32 = 0.95;
pub const RISK_PROSE_PRECISION: f32 = 0.90;
pub const COMMAND_PRECISION: f32 = 0.90;
pub const ACTION_PRECISION: f32 = 0.86;
pub const UNCERTAINTY_PRECISION: f32 = 0.80;
pub const DIM_PRECISION: f32 = 0.90;

pub const RISK_COMMAND_WEIGHT: f32 = 0.95;
pub const RISK_PROSE_WEIGHT: f32 = 0.85;
pub const COMMAND_WEIGHT: f32 = 0.75;
pub const ACTION_WEIGHT: f32 = 0.7;
pub const UNCERTAINTY_WEIGHT: f32 = 0.4;
pub const DIM_WEIGHT: f32 = 0.5;

const RISK_COMMAND_PATTERNS: [&str; 12] = [
    "rm -rf",
    "rm -fr",
    "sudo rm",
    "git reset --hard",
    "git push -f",
    "git push --force",
    "git clean -fd",
    "--force",
    "dd of=",
    "mkfs",
    "chmod -R 777",
    ":>",
];

const RISK_SQL_PATTERNS: [&str; 3] = ["DROP TABLE", "DROP DATABASE", "TRUNCATE"];

const RISK_PROSE_PATTERNS: [&str; 5] = [
    "irreversible",
    "cannot be undone",
    "can't be undone",
    "permanently delete",
    "truncate",
];

const UNCERTAINTY_PATTERNS: [&str; 11] = [
    "probably",
    "might",
    "may not",
    "I believe",
    "I think",
    "should work",
    "untested",
    "not sure",
    "unverified",
    "approximately",
    "roughly",
];

const COMMAND_BINARIES: [&str; 36] = [
    "git", "cargo", "npm", "pnpm", "yarn", "pip", "pip3", "python", "python3", "docker",
    "kubectl", "make", "rm", "mv", "cp", "mkdir", "ssh", "scp", "rsync", "curl", "wget",
    "systemctl", "journalctl", "nix", "nixos-rebuild", "apt", "brew", "chmod", "chown", "dd",
    "mkfs", "tar", "sed", "awk", "grep", "cat",
];

const PROSE_VETO_WORDS: [&str; 12] = [
    "is", "are", "was", "were", "will", "has", "have", "can", "and", "or", "the", "a",
];

static RISK_COMMAND_AC: LazyLock<AhoCorasick> = LazyLock::new(|| build_ac(&RISK_COMMAND_PATTERNS, false));
static RISK_SQL_AC: LazyLock<AhoCorasick> = LazyLock::new(|| build_ac(&RISK_SQL_PATTERNS, false));
static RISK_PROSE_AC: LazyLock<AhoCorasick> = LazyLock::new(|| build_ac(&RISK_PROSE_PATTERNS, true));
static UNCERTAINTY_AC: LazyLock<AhoCorasick> = LazyLock::new(|| build_ac(&UNCERTAINTY_PATTERNS, true));

static BACKTICK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+)`").expect("backtick regex"));
static DOLLAR_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s{0,8}\$\s+(\S.*)$").expect("dollar regex"));
static INDENTED_COMMAND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\s{2,})((?:sudo\s+)?[a-z][a-z0-9_.-]*)(\s+)(\S+)").expect("indented command regex")
});
static ACTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(you need to|you'll need to|you should now|run the following|make sure to|don't forget to|before you \w[^,]{0,60},|manually)")
        .expect("action regex")
});
static SHELL_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\$\s+)?(sudo\s+)?([a-z][a-z0-9_.-]*)(\s|$)").expect("shell shape regex")
});
static DIM_OPENER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(Let me\b|I'll\b|I'm going to\b|Now I\b|First,|Next,|Great!|Perfect!|Sure\b|Certainly\b)")
        .expect("dim opener regex")
});
static TOOL_NARRATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[⏺●○]\s|⎿\s|Tool call:|Calling tool\b|Running tool\b)").expect("tool narration regex")
});

fn build_ac(patterns: &[&str], case_insensitive: bool) -> AhoCorasick {
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(case_insensitive)
        .match_kind(MatchKind::LeftmostLongest)
        .build(patterns)
        .expect("heuristic automaton builds")
}

pub struct HeuristicScorer {
    toggles: crate::config::FacetToggles,
    dimensions: Option<DimensionRules>,
}

impl HeuristicScorer {
    pub fn new(config: &Config) -> Self {
        Self {
            toggles: config.facets,
            dimensions: DimensionRules::compile(&config.dimensions),
        }
    }

    pub fn score_line(&self, clean: &str) -> Vec<SpanScore> {
        if crate::score::self_report::is_annotation_line(clean) {
            return Vec::new();
        }
        let mut spans = Vec::new();
        if self.toggles.risk {
            risk_spans(clean, &mut spans);
        }
        if self.toggles.command {
            command_spans(clean, &mut spans);
        }
        if self.toggles.action {
            action_spans(clean, &mut spans);
        }
        if self.toggles.uncertainty {
            uncertainty_spans(clean, &mut spans);
        }
        if self.toggles.dim {
            dim_spans(clean, &mut spans);
        }
        if let Some(dimensions) = &self.dimensions {
            spans.extend(dimensions.spans(clean));
        }
        spans.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.range.start.cmp(&b.range.start)));
        spans.truncate(MAX_SPANS_PER_LINE);
        spans.sort_by_key(|s| s.range.start);
        spans
    }
}

impl Scorer for HeuristicScorer {
    fn name(&self) -> &'static str {
        "heuristic"
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Instant
    }

    fn score<'a>(&'a self, req: ScoreRequest<'a>) -> ScoreFuture<'a> {
        let spans = req
            .text
            .lines()
            .scan(0usize, |offset, line| {
                let base = *offset;
                *offset += line.len() + 1;
                Some((base, line))
            })
            .flat_map(|(base, line)| {
                self.score_line(line).into_iter().map(move |mut span| {
                    span.range = span.range.start + base..span.range.end + base;
                    span
                })
            })
            .collect();
        Box::pin(std::future::ready(Ok(spans)))
    }
}

fn push(spans: &mut Vec<SpanScore>, range: std::ops::Range<usize>, facet: Facet, weight: f32) {
    if range.start < range.end {
        spans.push(SpanScore::new(range, facet, weight, SourceId::Heuristic));
    }
}

fn risk_spans(clean: &str, spans: &mut Vec<SpanScore>) {
    for m in RISK_COMMAND_AC.find_iter(clean) {
        let pattern = RISK_COMMAND_PATTERNS[m.pattern().as_usize()];
        if pattern == "--force" && clean[m.end()..].starts_with('-') {
            continue;
        }
        if pattern == "mkfs" && !word_bounded_start(clean, m.start()) {
            continue;
        }
        push(spans, m.start()..m.end(), Facet::Risk, RISK_COMMAND_WEIGHT);
    }
    for m in RISK_SQL_AC.find_iter(clean) {
        if word_bounded(clean, m.start(), m.end()) {
            push(spans, m.start()..m.end(), Facet::Risk, RISK_COMMAND_WEIGHT);
        }
    }
    for m in RISK_PROSE_AC.find_iter(clean) {
        if word_bounded(clean, m.start(), m.end()) {
            push(spans, m.start()..m.end(), Facet::Risk, RISK_PROSE_WEIGHT);
        }
    }
}

fn word_bounded_start(text: &str, start: usize) -> bool {
    !text[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric())
}

fn command_spans(clean: &str, spans: &mut Vec<SpanScore>) {
    for caps in BACKTICK_RE.captures_iter(clean) {
        let content = caps.get(1).expect("backtick group");
        if is_shell_shape(content.as_str()) {
            push(spans, content.range(), Facet::Command, COMMAND_WEIGHT);
        }
    }
    if let Some(caps) = DOLLAR_LINE_RE.captures(clean) {
        let content = caps.get(1).expect("dollar group");
        push(spans, content.range(), Facet::Command, COMMAND_WEIGHT);
        return;
    }
    if let Some(caps) = INDENTED_COMMAND_RE.captures(clean) {
        let binary = caps.get(2).expect("binary group");
        let arg = caps.get(4).expect("arg group");
        let bare = binary.as_str().trim_start_matches("sudo").trim();
        if COMMAND_BINARIES.contains(&bare)
            && !PROSE_VETO_WORDS.contains(&arg.as_str())
            && arg.as_str().chars().next().is_some_and(|c| !c.is_uppercase())
        {
            push(spans, binary.start()..clean.trim_end().len(), Facet::Command, COMMAND_WEIGHT);
        }
    }
}

fn is_shell_shape(content: &str) -> bool {
    let Some(caps) = SHELL_SHAPE_RE.captures(content) else {
        return false;
    };
    if caps.get(1).is_some() || caps.get(2).is_some() {
        return true;
    }
    let binary = caps.get(3).expect("shape binary").as_str();
    let rest = content[caps.get(0).expect("shape").end()..].trim_start();
    let arg = rest.split_whitespace().next().unwrap_or("");
    COMMAND_BINARIES.contains(&binary)
        && (rest.is_empty() || !PROSE_VETO_WORDS.contains(&arg))
}

fn action_spans(clean: &str, spans: &mut Vec<SpanScore>) {
    for m in ACTION_RE.find_iter(clean) {
        if !at_sentence_start(clean, m.start()) {
            continue;
        }
        let tail = &clean[m.end()..];
        let sentence_end = tail
            .find(['.', '!', '?'])
            .map_or(clean.trim_end().len(), |i| m.end() + i);
        push(spans, m.start()..sentence_end, Facet::Action, ACTION_WEIGHT);
    }
}

fn at_sentence_start(text: &str, start: usize) -> bool {
    let head = text[..start].trim_end();
    head.is_empty() || head.ends_with(['.', '!', '?', ':', ';'])
}

fn uncertainty_spans(clean: &str, spans: &mut Vec<SpanScore>) {
    for m in UNCERTAINTY_AC.find_iter(clean) {
        if !word_bounded(clean, m.start(), m.end()) {
            continue;
        }
        let clause_start = clean[..m.start()]
            .rfind([',', ';', '.', ':'])
            .map_or(0, |i| i + 1);
        let clause_end = clean[m.end()..]
            .find([',', ';', '.', ':'])
            .map_or(clean.trim_end().len(), |i| m.end() + i);
        let start = clause_start + clean[clause_start..].len()
            - clean[clause_start..].trim_start().len();
        push(spans, start..clause_end, Facet::Uncertainty, UNCERTAINTY_WEIGHT);
    }
}

fn dim_spans(clean: &str, spans: &mut Vec<SpanScore>) {
    if DIM_OPENER_RE.is_match(clean) || TOOL_NARRATION_RE.is_match(clean) {
        let start = clean.len() - clean.trim_start().len();
        push(spans, start..clean.trim_end().len(), Facet::Dim, DIM_WEIGHT);
    }
}
